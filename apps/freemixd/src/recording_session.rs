//! Crash-safe program recording sessions for the native recorder slots.
//!
//! Every recorder segment the daemon opens is one `fm-record`
//! [`RecordingSession`] named after the segment's file stem, so
//! `program.mp4` is session `program` and `program-002.mp4` is session
//! `program-002`. The session's first file keeps the operator-visible name
//! (`program.mp4`); a write, sync, or creation failure re-homes the in-flight
//! fragment into `<stem>-NNNN.mp4` on the same directory (transient errors) or
//! on `--record-fallback-dir` (full, read-only, missing, or forbidden
//! directories). Each session keeps a `<stem>.fmsession` manifest in every
//! usable directory, and [`recover_interrupted`] repairs any session a crash
//! left open before the daemon records again.
//!
//! With `--record-wav` every segment also gets a WAV session of the same name
//! ([`SegmentFormat::Wav`]): `program.wav` first, `<stem>-NNNN.wav` after a
//! failover, and a `<stem>.wav.fmsession` manifest, under the same primary,
//! fallback, and free-space policy as the video session. Its records name the
//! session `<stem>.wav`.
//!
//! While a segment records, [`RemainingTimeWatch`] samples the active
//! directory's free space every few seconds and reports a
//! `FREEMIXD_RECORDER_REMAINING` record each time the estimated remaining
//! recording time crosses the configured thresholds (30, 10, and 2 minutes
//! by default; `FREEMIXD_RECORDER_REMAINING_MINUTES` takes a descending,
//! comma-separated list), hysteretically, and again when it clears. Every
//! live session holds an advisory lock on its name, so startup recovery
//! reports a session another daemon is still recording as `status=locked`
//! and leaves its files alone.
//!
//! Every record this module renders is path-free apart from the session name,
//! which is the operator's own file stem.

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use fm_codec_ffmpeg::record::{
    ResilientOutput, ResilientOutputState, ResilientOutputStatus, ResilientWavOutput,
    ResilientWavOutputState, ResilientWavOutputStatus,
};
use fm_record::{
    FileStorage, MAX_SESSION_NAME_BYTES, RecordingSession, RemainingTimeAlarm,
    RemainingTimeTransition, ResilientWavConfig, ResilientWriterConfig, SESSION_MANIFEST_EXTENSION,
    SESSION_WAV_SEGMENT_EXTENSION, SegmentFormat, SegmentOutcome, SessionConfig, SessionError,
    SessionErrorRecord, SessionManifest, SessionRecovery, SessionSnapshot, SessionState,
    SpaceLevel, SpacePreflight, SpaceThresholds, WavSegmentOutcome, WavSessionRecovery,
    estimate_recording_space, preflight_recording_space, recover_session, recover_wav_session,
};

use crate::disk;

/// Bytes a derived `-NNN` ordinal adds to a configured stem.
const ORDINAL_SUFFIX_BYTES: usize = 4;
/// Longest configured output stem: derived segment names must stay valid
/// session names.
pub(crate) const MAX_OUTPUT_STEM_BYTES: usize = MAX_SESSION_NAME_BYTES - ORDINAL_SUFFIX_BYTES;
/// Remaining recording time a root must cover to count as sufficient.
const PREFLIGHT_MINIMUM: Duration = Duration::from_mins(5);
/// AAC rate assumed per channel when the slot keeps the encoder default.
const DEFAULT_AUDIO_BITS_PER_CHANNEL: u64 = 64_000;
/// libx264 bits per pixel assumed when the slot keeps the encoder default.
const DEFAULT_VIDEO_MILLIBITS_PER_PIXEL: u64 = 150;
/// Remaining recording times whose crossing is reported while recording,
/// unless [`REMAINING_THRESHOLDS_ENV`] overrides them.
const REMAINING_THRESHOLDS: [Duration; 3] = [
    Duration::from_mins(30),
    Duration::from_mins(10),
    Duration::from_mins(2),
];
/// Environment variable naming the remaining-time warning thresholds, in
/// whole minutes, descending and comma separated (1 through 1440 each, at
/// most [`fm_record::MAX_REMAINING_THRESHOLDS`]). An invalid list keeps the
/// defaults.
pub(crate) const REMAINING_THRESHOLDS_ENV: &str = "FREEMIXD_RECORDER_REMAINING_MINUTES";
/// How far above a crossed threshold remaining time must climb to clear it.
const REMAINING_CLEAR_MARGIN: Duration = Duration::from_mins(1);
/// Spacing of the free-space samples behind remaining-time records.
const REMAINING_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// The configured remaining-time thresholds, or [`REMAINING_THRESHOLDS`]
/// when the environment names none or an invalid list.
fn configured_remaining_thresholds() -> Vec<Duration> {
    std::env::var(REMAINING_THRESHOLDS_ENV)
        .ok()
        .and_then(|value| parse_remaining_thresholds(&value))
        .unwrap_or_else(|| REMAINING_THRESHOLDS.to_vec())
}

/// Parses `minutes` as a descending, unique, comma-separated list of whole
/// minutes in `1..=1440`, at most [`fm_record::MAX_REMAINING_THRESHOLDS`].
fn parse_remaining_thresholds(minutes: &str) -> Option<Vec<Duration>> {
    let minutes: Vec<u64> = minutes
        .split(',')
        .map(str::trim)
        .map(|token| token.parse::<u64>().ok())
        .collect::<Option<Vec<u64>>>()?;
    if minutes.is_empty()
        || minutes.len() > fm_record::MAX_REMAINING_THRESHOLDS
        || minutes.iter().any(|minutes| !(1..=1_440).contains(minutes))
        || minutes.windows(2).any(|pair| pair[0] <= pair[1])
    {
        return None;
    }
    Some(minutes.into_iter().map(Duration::from_mins).collect())
}

/// Segment storage used for every native recording session: plain files,
/// with free space measured through the daemon's own probe.
pub(crate) type SessionStorage = FileStorage<fn(&Path) -> io::Result<u64>>;

/// Checks that a configured `--record-program`/`--record-program-2` path
/// yields valid session names for itself and every derived `-NNN` segment.
///
/// # Errors
///
/// Returns a static, path-free reason.
pub(crate) fn validate_output_stem(path: &Path) -> Result<(), &'static str> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("output stem is not valid Unicode")?;
    if stem.len() > MAX_OUTPUT_STEM_BYTES {
        return Err("output stem is longer than 60 bytes");
    }
    SessionConfig::new(stem, ".").map(|_| ()).map_err(|_| {
        "output stem must be ASCII letters, digits, `-`, `_`, or `.` and must not start with `.`"
    })
}

/// The session name of one segment path: its file stem.
fn session_name(segment: &Path) -> Option<&str> {
    segment.file_stem().and_then(|stem| stem.to_str())
}

fn manifest_file_name(name: &str) -> String {
    format!("{name}.{SESSION_MANIFEST_EXTENSION}")
}

fn wav_manifest_file_name(name: &str) -> String {
    format!("{name}.{SESSION_WAV_SEGMENT_EXTENSION}.{SESSION_MANIFEST_EXTENSION}")
}

fn wav_file_name(name: &str) -> String {
    format!("{name}.{SESSION_WAV_SEGMENT_EXTENSION}")
}

/// The name WAV session records carry: `<stem>.wav`.
fn wav_record_name(name: &str) -> String {
    wav_file_name(name)
}

/// Segment-name bookkeeping shared by every recorder slot: the optional
/// fallback directory and the names whose sessions are still being started
/// off the daemon loop.
#[derive(Clone, Debug, Default)]
pub(crate) struct SegmentNames {
    fallback: Option<PathBuf>,
    reserved: Arc<Mutex<HashSet<PathBuf>>>,
}

impl SegmentNames {
    pub(crate) fn new(fallback: Option<PathBuf>) -> Self {
        Self {
            fallback,
            reserved: Arc::default(),
        }
    }

    pub(crate) fn fallback(&self) -> Option<&Path> {
        self.fallback.as_deref()
    }

    /// Whether `candidate` cannot name a new segment: its file or its
    /// `<stem>.wav` sidecar exists in the primary or fallback directory, a
    /// video or WAV session manifest of that name exists in either, or a
    /// session of that name is still starting.
    pub(crate) fn taken(&self, candidate: &Path) -> bool {
        if candidate.exists() {
            return true;
        }
        let Some(name) = session_name(candidate) else {
            return false;
        };
        let primary = candidate
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut files = vec![
            manifest_file_name(name),
            wav_manifest_file_name(name),
            wav_file_name(name),
        ];
        if any_exists(primary, &files) {
            return true;
        }
        if let Some(file) = candidate.file_name().and_then(|file| file.to_str()) {
            files.push(file.to_owned());
        }
        if let Some(fallback) = &self.fallback
            && any_exists(fallback, &files)
        {
            return true;
        }
        self.reserved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(candidate)
    }

    fn reserve(&self, segment: &Path) -> Reservation {
        self.reserved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(segment.to_path_buf());
        Reservation {
            reserved: Arc::clone(&self.reserved),
            segment: segment.to_path_buf(),
        }
    }
}

/// Whether any of `files` exists in `directory`.
fn any_exists(directory: &Path, files: &[String]) -> bool {
    files.iter().any(|file| directory.join(file).exists())
}

/// Holds a segment name until its session manifest exists on disk.
#[derive(Debug)]
struct Reservation {
    reserved: Arc<Mutex<HashSet<PathBuf>>>,
    segment: PathBuf,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.reserved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.segment);
    }
}

/// Builds the session configuration for `format` segments under `primary`.
/// Video and WAV sessions share every placement rule.
fn session_config(
    name: &str,
    primary: &Path,
    fallback: Option<&Path>,
    format: SegmentFormat,
) -> Result<SessionConfig, String> {
    let mut config = SessionConfig::new(name, primary)
        .map_err(|error| error.to_string())?
        .with_segment_format(format)
        .with_bare_first_segment()
        .with_min_free_bytes(disk::RECORDING_MINIMUM_FREE_BYTES);
    if let Some(fallback) = fallback.filter(|fallback| !same_directory(primary, fallback)) {
        config = config
            .with_fallback(fallback)
            .map_err(|error| error.to_string())?;
    }
    Ok(config)
}

fn same_directory(left: &Path, right: &Path) -> bool {
    left == right
        || matches!(
            (fs::canonicalize(left), fs::canonicalize(right)),
            (Ok(left), Ok(right)) if left == right
        )
}

/// A session chosen on the daemon loop and started later, possibly on a
/// worker. Its name stays reserved until [`open`] has written the manifest.
#[derive(Debug)]
pub(crate) struct PlannedSession {
    config: SessionConfig,
    /// The WAV sidecar session, with `--record-wav`.
    wav: Option<SessionConfig>,
    slot: usize,
    bitrate_bits_per_second: u64,
    _reservation: Reservation,
}

#[cfg(test)]
impl PlannedSession {
    pub(crate) fn config(&self) -> &SessionConfig {
        &self.config
    }

    pub(crate) fn wav_config(&self) -> Option<&SessionConfig> {
        self.wav.as_ref()
    }
}

/// Plans the session for `segment`, whose canonical parent is `primary`.
/// `wav_bits_per_second` (see [`wav_bitrate`]) adds a WAV sidecar session of
/// the same name, and its rate to the free-space preflight.
///
/// # Errors
///
/// Returns a path-free reason for an invalid session name.
pub(crate) fn plan(
    segment: &Path,
    primary: &Path,
    names: &SegmentNames,
    slot: usize,
    bitrate_bits_per_second: u64,
    wav_bits_per_second: Option<u64>,
) -> Result<PlannedSession, String> {
    let name = session_name(segment).ok_or("recording output stem is not valid Unicode")?;
    let config = session_config(name, primary, names.fallback(), SegmentFormat::Fmp4)?;
    let wav = wav_bits_per_second
        .map(|_| session_config(name, primary, names.fallback(), SegmentFormat::Wav))
        .transpose()?;
    Ok(PlannedSession {
        config,
        wav,
        slot,
        bitrate_bits_per_second: bitrate_bits_per_second
            .saturating_add(wav_bits_per_second.unwrap_or(0)),
        _reservation: names.reserve(segment),
    })
}

/// Bit rate of the 16-bit linear-PCM WAV sidecar.
pub(crate) fn wav_bitrate(sample_rate: u32, channels: usize) -> u64 {
    u64::from(sample_rate)
        .saturating_mul(channels.max(1) as u64)
        .saturating_mul(16)
}

/// Estimated recording bitrate in bits per second, including a small
/// container overhead. Configured bitrates are used as-is; encoder defaults
/// are estimated from the frame size and channel count.
pub(crate) fn estimated_bitrate(
    width: u32,
    height: u32,
    rate: fm_types::FrameRate,
    channels: usize,
    video_kbps: Option<u32>,
    audio_kbps: Option<u32>,
) -> u64 {
    let video = video_kbps.map_or_else(
        || {
            let pixels_per_second =
                u128::from(width) * u128::from(height) * u128::from(rate.numerator())
                    / u128::from(rate.denominator().max(1));
            u64::try_from(pixels_per_second * u128::from(DEFAULT_VIDEO_MILLIBITS_PER_PIXEL) / 1000)
                .unwrap_or(u64::MAX)
        },
        |kbps| u64::from(kbps) * 1000,
    );
    let audio = audio_kbps.map_or_else(
        || DEFAULT_AUDIO_BITS_PER_CHANNEL.saturating_mul(channels.max(1) as u64),
        |kbps| u64::from(kbps) * 1000,
    );
    let payload = video.saturating_add(audio);
    payload.saturating_add(payload / 32)
}

fn probe(directory: &Path) -> io::Result<u64> {
    disk::available_bytes(disk::existing_ancestor(directory).unwrap_or(directory))
}

/// The started outputs of one planned segment and their shared status.
pub(crate) struct OpenedSession {
    pub(crate) output: ResilientOutput<SessionStorage>,
    /// The WAV sidecar sink, with `--record-wav`.
    pub(crate) wav: Option<ResilientWavOutput<SessionStorage>>,
    pub(crate) status: SessionStatus,
}

/// Runs the free-space preflight, starts the session (and its WAV sidecar
/// session), and wraps each in its fault-tolerant recorder output. Reports
/// the preflight on stderr.
///
/// # Errors
///
/// Refuses when every root with a known free-space figure is exhausted, or
/// when a session cannot start (no usable directory, an existing manifest
/// of the same name, or a manifest that reaches no directory). A refused WAV
/// session discards the video session it would have accompanied.
pub(crate) fn open(planned: PlannedSession) -> Result<OpenedSession, String> {
    // The reservation is held until both manifests exist.
    let PlannedSession {
        config,
        wav,
        slot,
        bitrate_bits_per_second,
        _reservation,
    } = planned;
    let mut space_probe = probe;
    let preflight = preflight_recording_space(
        &mut space_probe,
        config.roots(),
        bitrate_bits_per_second,
        PREFLIGHT_MINIMUM,
        &SpaceThresholds::default(),
    );
    eprintln!(
        "{}",
        space_record(slot, config.name(), bitrate_bits_per_second, &preflight)
    );
    if preflight_refuses(&preflight) {
        return Err("preflight:no recording directory has usable free space".to_owned());
    }
    let storage: SessionStorage = FileStorage::new(disk::available_bytes);
    let session = RecordingSession::start(config.clone(), storage)
        .map_err(|error| format!("session:{error}"))?;
    let output = ResilientOutput::new(session, ResilientWriterConfig::default());
    let mut status = SessionStatus {
        config,
        video: output.status(),
        wav: None,
        bitrate_bits_per_second,
    };
    let Some(wav_config) = wav else {
        return Ok(OpenedSession {
            output,
            wav: None,
            status,
        });
    };
    let storage: SessionStorage = FileStorage::new(disk::available_bytes);
    match RecordingSession::start(wav_config.clone(), storage) {
        Ok(session) => {
            let wav = ResilientWavOutput::new(session, ResilientWavConfig::default());
            status.wav = Some(WavStatus {
                config: wav_config,
                status: wav.status(),
            });
            Ok(OpenedSession {
                output,
                wav: Some(wav),
                status,
            })
        }
        Err(error) => {
            drop(output);
            discard_unused(&status);
            Err(format!("wav_session:{error}"))
        }
    }
}

/// The WAV half of a [`SessionStatus`].
#[derive(Clone, Debug)]
struct WavStatus {
    config: SessionConfig,
    status: ResilientWavOutputStatus,
}

/// Read handle on one open segment's video session and, with
/// `--record-wav`, its WAV sidecar session.
#[derive(Clone, Debug)]
pub(crate) struct SessionStatus {
    config: SessionConfig,
    video: ResilientOutputStatus,
    wav: Option<WavStatus>,
    /// Estimated combined video and WAV bitrate.
    bitrate_bits_per_second: u64,
}

/// A consistent copy of each session's latest state.
#[derive(Clone, Debug)]
pub(crate) struct SessionStates {
    pub(crate) video: ResilientOutputState,
    pub(crate) wav: Option<ResilientWavOutputState>,
}

impl SessionStatus {
    /// The session name: the segment's file stem.
    pub(crate) fn name(&self) -> &str {
        self.config.name()
    }

    /// The video session's configuration.
    #[cfg(test)]
    pub(crate) fn config(&self) -> &SessionConfig {
        &self.config
    }

    pub(crate) fn get(&self) -> SessionStates {
        SessionStates {
            video: self.video.get(),
            wav: self.wav.as_ref().map(|wav| wav.status.get()),
        }
    }
}

/// Unknown free space never blocks; a start is refused only when every
/// measured root is exhausted.
fn preflight_refuses(preflight: &SpacePreflight) -> bool {
    let mut measured = preflight
        .roots
        .iter()
        .filter_map(|root| root.estimate)
        .peekable();
    measured.peek().is_some() && measured.all(|estimate| estimate.level == SpaceLevel::Exhausted)
}

fn space_record(slot: usize, name: &str, bitrate: u64, preflight: &SpacePreflight) -> String {
    let roots = preflight
        .roots
        .iter()
        .map(|root| match (root.estimate, root.probe_error) {
            (Some(estimate), _) => format!(
                "{}:{:?}:{}",
                root.root,
                estimate.level,
                estimate
                    .remaining
                    .map_or_else(|| "unbounded".to_owned(), |left| left.as_secs().to_string())
            ),
            (None, kind) => format!(
                "{}:unknown:{}",
                root.root,
                kind.map_or_else(|| "none".to_owned(), |kind| format!("{kind:?}"))
            ),
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "FREEMIXD_RECORDER_SPACE\tv=1\tslot={slot}\tsession={}\tbitrate_bps={bitrate}\tminimum_s={}\tsufficient_root={}\troots={roots}",
        field(name),
        preflight.minimum.as_secs(),
        preflight
            .first_sufficient_root()
            .map_or_else(|| "none".to_owned(), |root| root.to_string()),
    )
}

/// Removes the manifests of every session (video and WAV) that never
/// created a segment, so a refused start leaves nothing behind and its name
/// stays free.
pub(crate) fn discard_unused(status: &SessionStatus) {
    if status.video.get().segments.is_empty() {
        remove_manifests(&status.config);
    }
    if let Some(wav) = &status.wav
        && wav.status.get().segments.is_empty()
    {
        remove_manifests(&wav.config);
    }
}

fn remove_manifests(config: &SessionConfig) {
    let manifest = config.manifest_file_name();
    for root in config.roots() {
        let _ = fs::remove_file(root.join(&manifest));
    }
}

/// Live failover detection for one open session and its WAV sidecar.
#[derive(Debug, Default)]
pub(crate) struct FailoverWatch {
    disabled: usize,
    wav_disabled: usize,
}

impl FailoverWatch {
    /// Returns a `FREEMIXD_RECORDER_FAILOVER` record for each session (video,
    /// then the WAV sidecar as `<name>.wav`) that disabled a directory since
    /// the last observation, one per line.
    pub(crate) fn observe(
        &mut self,
        slot: usize,
        name: &str,
        state: &SessionStates,
    ) -> Option<String> {
        let video = failover_record(slot, name, &state.video.snapshot, &mut self.disabled);
        let wav = state.wav.as_ref().and_then(|wav| {
            failover_record(
                slot,
                &wav_record_name(name),
                &wav.snapshot,
                &mut self.wav_disabled,
            )
        });
        match (video, wav) {
            (Some(video), Some(wav)) => Some(format!("{video}\n{wav}")),
            (video, wav) => video.or(wav),
        }
    }
}

fn failover_record(
    slot: usize,
    name: &str,
    snapshot: &SessionSnapshot,
    seen: &mut usize,
) -> Option<String> {
    let disabled = snapshot.disabled_roots.len();
    if disabled < *seen {
        // A directory disabled for space had room again and was re-enabled.
        *seen = disabled;
        return Some(format!(
            "FREEMIXD_RECORDER_RESTORED\tv=1\tslot={slot}\tsession={}\tstate={:?}\tactive_root={}\tdisabled_roots={}",
            field(name),
            snapshot.state,
            snapshot.active_root,
            root_list(&snapshot.disabled_roots),
        ));
    }
    if disabled == *seen {
        return None;
    }
    *seen = disabled;
    Some(format!(
        "FREEMIXD_RECORDER_FAILOVER\tv=1\tslot={slot}\tsession={}\tstate={:?}\tactive_root={}\tdisabled_roots={}\tcause={}",
        field(name),
        snapshot.state,
        snapshot.active_root,
        root_list(&snapshot.disabled_roots),
        error_field(snapshot.errors.last()),
    ))
}

/// Hysteretic remaining-recording-time warnings for one open session.
#[derive(Debug)]
pub(crate) struct RemainingTimeWatch {
    alarm: RemainingTimeAlarm,
    sampled_at: Option<Instant>,
}

impl Default for RemainingTimeWatch {
    fn default() -> Self {
        Self::with_thresholds(configured_remaining_thresholds(), REMAINING_CLEAR_MARGIN)
    }
}

impl RemainingTimeWatch {
    /// A watch reporting crossings of `thresholds` (sorted and bounded by
    /// [`RemainingTimeAlarm`]) that clear by `clear_margin`.
    #[allow(clippy::needless_pass_by_value)] // callers hand over a fresh list
    fn with_thresholds(thresholds: Vec<Duration>, clear_margin: Duration) -> Self {
        Self {
            alarm: RemainingTimeAlarm::new(&thresholds, clear_margin),
            sampled_at: None,
        }
    }

    /// Samples the active directory's free space at most every
    /// [`REMAINING_SAMPLE_INTERVAL`] and returns a
    /// `FREEMIXD_RECORDER_REMAINING` record when the estimated remaining time
    /// crosses a threshold. A failed free-space query is skipped.
    pub(crate) fn observe(
        &mut self,
        slot: usize,
        status: &SessionStatus,
        now: Instant,
    ) -> Option<String> {
        if self
            .sampled_at
            .is_some_and(|last| now.saturating_duration_since(last) < REMAINING_SAMPLE_INTERVAL)
        {
            return None;
        }
        self.sampled_at = Some(now);
        let active_root = status.video.get().snapshot.active_root;
        let directory = status.config.roots().get(active_root)?;
        let available = probe(directory).ok()?;
        self.observe_sample(
            slot,
            status.name(),
            active_root,
            available,
            status.bitrate_bits_per_second,
        )
    }

    /// Evaluates one free-space sample of the active root.
    fn observe_sample(
        &mut self,
        slot: usize,
        name: &str,
        active_root: usize,
        available_bytes: u64,
        bitrate_bits_per_second: u64,
    ) -> Option<String> {
        let estimate = estimate_recording_space(
            available_bytes,
            bitrate_bits_per_second,
            &SpaceThresholds::default(),
        );
        let transition = self.alarm.observe(estimate.remaining)?;
        Some(remaining_record(
            slot,
            name,
            active_root,
            available_bytes,
            bitrate_bits_per_second,
            &transition,
        ))
    }
}

fn remaining_record(
    slot: usize,
    name: &str,
    active_root: usize,
    available_bytes: u64,
    bitrate: u64,
    transition: &RemainingTimeTransition,
) -> String {
    let seconds = |threshold: Option<Duration>| {
        threshold.map_or_else(|| "none".to_owned(), |value| value.as_secs().to_string())
    };
    format!(
        "FREEMIXD_RECORDER_REMAINING\tv=1\tslot={slot}\tsession={}\tstate={}\tthreshold_s={}\tprevious_threshold_s={}\tremaining_s={}\tactive_root={active_root}\tfree_bytes={available_bytes}\tbitrate_bps={bitrate}",
        field(name),
        if transition.to.is_some() {
            "low"
        } else {
            "normal"
        },
        seconds(transition.to),
        seconds(transition.from),
        transition
            .remaining
            .map_or_else(|| "unbounded".to_owned(), |left| left.as_secs().to_string()),
    )
}

/// One `FREEMIXD_RECORDER_SESSION` record for a finished segment's session,
/// followed on the next line by a `FREEMIXD_RECORDER_WAV_SESSION` record for
/// its WAV sidecar session, if any.
pub(crate) fn session_report(slot: usize, name: &str, state: &SessionStates) -> String {
    let video = video_session_report(slot, name, &state.video);
    match &state.wav {
        Some(wav) => format!("{video}\n{}", wav_session_report(slot, name, wav)),
        None => video,
    }
}

/// One `FREEMIXD_RECORDER_WAV_SESSION` record, naming the session
/// `<name>.wav`.
fn wav_session_report(slot: usize, name: &str, state: &ResilientWavOutputState) -> String {
    let counters = state.stats;
    format!(
        "FREEMIXD_RECORDER_WAV_SESSION\tv=1\tslot={slot}\tsession={}\tstate={:?}\tfinished={}\tsegments={}\tframes={}\tdata_bytes={}\tbytes={}\tfailovers={}\tretries={}\trewritten_bytes={}\theader_patch_failures={}\tactive_root={}\tdisabled_roots={}\terrors={}\terrors_dropped={}\tlast_error={}\tdiscarded_tail_bytes={}\tmanifest_stale={}\tfailure={}",
        field(&wav_record_name(name)),
        state.snapshot.state,
        state.finished,
        state.segments.len(),
        counters.frames_written,
        counters.data_bytes,
        counters.bytes_written,
        counters.failovers,
        counters.retries,
        counters.bytes_rewritten,
        counters.header_patch_failures,
        state.snapshot.active_root,
        root_list(&state.snapshot.disabled_roots),
        state.snapshot.errors.len(),
        state.snapshot.errors_dropped,
        error_field(state.snapshot.errors.last()),
        state.discarded_tail_bytes,
        state.snapshot.manifest_stale,
        state
            .error
            .as_deref()
            .map_or_else(|| "none".to_owned(), field),
    )
}

fn video_session_report(slot: usize, name: &str, state: &ResilientOutputState) -> String {
    let counters = state.stats;
    format!(
        "FREEMIXD_RECORDER_SESSION\tv=1\tslot={slot}\tsession={}\tstate={:?}\tfinished={}\tsegments={}\tfragments={}\tbytes={}\tfailovers={}\tretries={}\trewritten_fragments={}\tactive_root={}\tdisabled_roots={}\terrors={}\terrors_dropped={}\tlast_error={}\tdiscarded_tail_bytes={}\tmanifest_stale={}\tfailure={}",
        field(name),
        state.snapshot.state,
        state.finished,
        state.segments.len(),
        counters.fragments_written,
        counters.bytes_written,
        counters.failovers,
        counters.retries,
        counters.fragments_rewritten,
        state.snapshot.active_root,
        root_list(&state.snapshot.disabled_roots),
        state.snapshot.errors.len(),
        state.snapshot.errors_dropped,
        error_field(state.snapshot.errors.last()),
        state.discarded_tail_bytes,
        state.snapshot.manifest_stale,
        state
            .error
            .as_deref()
            .map_or_else(|| "none".to_owned(), field),
    )
}

fn root_list(roots: &[usize]) -> String {
    if roots.is_empty() {
        return "none".to_owned();
    }
    roots
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn error_field(error: Option<&SessionErrorRecord>) -> String {
    error.map_or_else(
        || "none".to_owned(),
        |error| {
            format!(
                "{:?}:{:?}@{}x{}",
                error.op, error.kind, error.root, error.count
            )
        },
    )
}

/// Tabs and control characters never reach a record.
fn field(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\t' | '\r' | '\n' => ' ',
            character if character.is_control() => '?',
            character => character,
        })
        .collect()
}

/// One configured recorder slot whose directories startup recovery scans.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RecoveryScope<'a> {
    /// 1-based slot number.
    pub(crate) slot: usize,
    /// The configured `--record-program`/`--record-program-2` path.
    pub(crate) output: &'a Path,
}

/// Whether `name` is the configured stem or one of its derived `-NNN`
/// segment names.
fn derives_from(name: &str, stem: &str) -> bool {
    name == stem
        || name
            .strip_prefix(stem)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|digits| digits.len() == 3 && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// The newest intact manifest copy across `config`'s roots.
fn newest_manifest(config: &SessionConfig) -> Option<SessionManifest> {
    config
        .roots()
        .iter()
        .filter_map(|root| fs::read_to_string(root.join(config.manifest_file_name())).ok())
        .filter_map(|text| SessionManifest::parse(&text).ok())
        .filter(|manifest| manifest.name == config.name())
        .max_by_key(|manifest| manifest.generation)
}

/// Finds every session of the configured slots that a crash left open and
/// recovers it: torn segment tails are truncated to their last complete
/// fragment and the manifest is rewritten as recovered. Returns one record
/// per recovered session.
///
/// A session whose newest manifest says it stopped, failed, or was already
/// recovered is left alone, and so is one another live recorder holds
/// (reported `status=locked`). Sessions are found by manifest in the slot's
/// output directory and the fallback directory, and only under names the
/// slot itself derives, so another recorder's files are never touched.
pub(crate) fn recover_interrupted(
    scopes: &[RecoveryScope<'_>],
    fallback: Option<&Path>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    for scope in scopes {
        let Some(stem) = session_name(scope.output) else {
            continue;
        };
        let primary = scope
            .output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let primary = fs::canonicalize(primary).unwrap_or_else(|_| primary.to_path_buf());
        let mut names: Vec<(String, SegmentFormat)> = [Some(primary.as_path()), fallback]
            .into_iter()
            .flatten()
            .filter_map(|directory| fs::read_dir(directory).ok())
            .flat_map(Iterator::flatten)
            .filter_map(|entry| {
                let file_name = entry.file_name().into_string().ok()?;
                manifest_session(&file_name, stem)
            })
            .collect();
        names.sort_by(|(left, left_format), (right, right_format)| {
            (left, *left_format == SegmentFormat::Wav)
                .cmp(&(right, *right_format == SegmentFormat::Wav))
        });
        names.dedup();
        for (name, format) in names {
            if !seen.insert((primary.clone(), name.clone(), format)) {
                continue;
            }
            let config = match session_config(&name, &primary, fallback, format) {
                Ok(config) => config,
                Err(error) => {
                    records.push(recovery_failure(scope.slot, &name, format, &error));
                    continue;
                }
            };
            let newest = newest_manifest(&config);
            if newest
                .as_ref()
                .is_some_and(|manifest| manifest.state != SessionState::Recording)
            {
                continue;
            }
            let record = match format {
                SegmentFormat::Fmp4 => recover_session(&config)
                    .map(|recovery| recovery_record(scope.slot, &name, &recovery)),
                // A WAV session is never handed to the MP4 scanner.
                SegmentFormat::Wav => recover_wav_session(&config)
                    .map(|recovery| wav_recovery_record(scope.slot, &name, &recovery)),
            };
            records.push(record.unwrap_or_else(|error| match error {
                SessionError::Locked { .. } => recovery_locked(scope.slot, &name, format),
                error => recovery_failure(scope.slot, &name, format, &error.to_string()),
            }));
        }
    }
    records
}

/// The session a manifest file belongs to, when it is one of `stem`'s:
/// `<name>.wav.fmsession` is a WAV session (checked first), and
/// `<name>.fmsession` a video session.
fn manifest_session(file_name: &str, stem: &str) -> Option<(String, SegmentFormat)> {
    let name = file_name
        .strip_suffix(SESSION_MANIFEST_EXTENSION)?
        .strip_suffix('.')?;
    if let Some(wav) = name
        .strip_suffix(SESSION_WAV_SEGMENT_EXTENSION)
        .and_then(|name| name.strip_suffix('.'))
        && derives_from(wav, stem)
    {
        return Some((wav.to_owned(), SegmentFormat::Wav));
    }
    derives_from(name, stem).then(|| (name.to_owned(), SegmentFormat::Fmp4))
}

/// A session another live recorder (or recovery) still holds: its files are
/// left alone.
fn recovery_locked(slot: usize, name: &str, format: SegmentFormat) -> String {
    let (record, name) = match format {
        SegmentFormat::Fmp4 => ("FREEMIXD_RECORDING_RECOVERY", name.to_owned()),
        SegmentFormat::Wav => ("FREEMIXD_RECORDING_WAV_RECOVERY", wav_record_name(name)),
    };
    format!(
        "{record}\tv=1\tslot={slot}\tsession={}\tstatus=locked",
        field(&name)
    )
}

fn recovery_failure(slot: usize, name: &str, format: SegmentFormat, error: &str) -> String {
    let (record, name) = match format {
        SegmentFormat::Fmp4 => ("FREEMIXD_RECORDING_RECOVERY", name.to_owned()),
        SegmentFormat::Wav => ("FREEMIXD_RECORDING_WAV_RECOVERY", wav_record_name(name)),
    };
    format!(
        "{record}\tv=1\tslot={slot}\tsession={}\tstatus=failed\tfailure={}",
        field(&name),
        field(error)
    )
}

/// One `FREEMIXD_RECORDING_WAV_RECOVERY` record, naming the session
/// `<name>.wav`: what a WAV sidecar crash recovery kept and what it lost.
pub(crate) fn wav_recovery_record(
    slot: usize,
    name: &str,
    recovery: &WavSessionRecovery,
) -> String {
    let mut intact = 0;
    let mut repaired = 0;
    let mut unrecoverable = 0;
    let mut missing = 0;
    for segment in &recovery.segments {
        match segment.outcome {
            WavSegmentOutcome::Intact { .. } => intact += 1,
            WavSegmentOutcome::Repaired { .. } => repaired += 1,
            WavSegmentOutcome::Unrecoverable { .. } => unrecoverable += 1,
            WavSegmentOutcome::Missing | WavSegmentOutcome::Unreadable { .. } => missing += 1,
            WavSegmentOutcome::NeverCreated => {}
        }
    }
    format!(
        "FREEMIXD_RECORDING_WAV_RECOVERY\tv=1\tslot={slot}\tsession={}\tstatus=recovered\tprevious_state={}\tsegments={}\tintact={intact}\trepaired={repaired}\tunrecoverable={unrecoverable}\tmissing={missing}\tkept_bytes={}\tdiscarded_bytes={}\trecovered_ms={}\tunknown_loss_segments={}\tmanifest_generation={}",
        field(&wav_record_name(name)),
        recovery
            .previous_state()
            .map_or_else(|| "unknown".to_owned(), |state| format!("{state:?}")),
        recovery.segments.len(),
        recovery.kept_bytes(),
        recovery.discarded_bytes(),
        recovery.recovered_duration_nanos() / 1_000_000,
        recovery.segments_with_unknown_loss(),
        recovery
            .rewritten_generation
            .map_or_else(|| "none".to_owned(), |generation| generation.to_string()),
    )
}

/// One `FREEMIXD_RECORDING_RECOVERY` record: what a crash recovery kept and
/// what it lost.
pub(crate) fn recovery_record(slot: usize, name: &str, recovery: &SessionRecovery) -> String {
    let mut intact = 0;
    let mut repaired = 0;
    let mut unrecoverable = 0;
    let mut missing = 0;
    for segment in &recovery.segments {
        match segment.outcome {
            SegmentOutcome::Intact { .. } => intact += 1,
            SegmentOutcome::Repaired { .. } => repaired += 1,
            SegmentOutcome::Unrecoverable { .. } => unrecoverable += 1,
            SegmentOutcome::Missing | SegmentOutcome::Unreadable { .. } => missing += 1,
            SegmentOutcome::NeverCreated => {}
        }
    }
    format!(
        "FREEMIXD_RECORDING_RECOVERY\tv=1\tslot={slot}\tsession={}\tstatus=recovered\tprevious_state={}\tsegments={}\tintact={intact}\trepaired={repaired}\tunrecoverable={unrecoverable}\tmissing={missing}\tkept_bytes={}\tdiscarded_bytes={}\trecovered_ms={}\tknown_lost_ms={}\tunknown_loss_segments={}\tmanifest_generation={}",
        field(name),
        recovery
            .previous_state()
            .map_or_else(|| "unknown".to_owned(), |state| format!("{state:?}")),
        recovery.segments.len(),
        recovery.kept_bytes(),
        recovery.discarded_bytes(),
        recovery.recovered_duration_nanos() / 1_000_000,
        recovery.known_lost_duration_nanos() / 1_000_000,
        recovery.segments_with_unknown_loss(),
        recovery
            .rewritten_generation
            .map_or_else(|| "none".to_owned(), |generation| generation.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_stems_must_yield_valid_session_names() {
        for valid in ["program.mp4", "show_1.take-2.mp4", "dir/x.mp4"] {
            assert_eq!(validate_output_stem(Path::new(valid)), Ok(()), "{valid}");
        }
        let long = format!("{}.mp4", "x".repeat(MAX_OUTPUT_STEM_BYTES));
        assert_eq!(validate_output_stem(Path::new(&long)), Ok(()));
        let too_long = format!("{}.mp4", "x".repeat(MAX_OUTPUT_STEM_BYTES + 1));
        assert!(validate_output_stem(Path::new(&too_long)).is_err());
        for invalid in ["my show.mp4", ".hidden.mp4", "caf\u{e9}.mp4"] {
            assert!(
                validate_output_stem(Path::new(invalid)).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn names_are_taken_by_files_manifests_fallback_copies_and_reservations() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        let fallback = directory.path().join("fallback");
        fs::create_dir_all(&primary).unwrap();
        fs::create_dir_all(&fallback).unwrap();
        let names = SegmentNames::new(Some(fallback.clone()));
        let candidate = primary.join("program.mp4");
        assert!(!names.taken(&candidate));

        fs::write(primary.join("program.fmsession"), b"").unwrap();
        assert!(names.taken(&candidate), "a primary manifest takes the name");
        fs::remove_file(primary.join("program.fmsession")).unwrap();

        fs::write(fallback.join("program.fmsession"), b"").unwrap();
        assert!(
            names.taken(&candidate),
            "a fallback manifest takes the name"
        );
        fs::remove_file(fallback.join("program.fmsession")).unwrap();

        fs::write(fallback.join("program.mp4"), b"").unwrap();
        assert!(names.taken(&candidate), "a fallback segment takes the name");
        fs::remove_file(fallback.join("program.mp4")).unwrap();

        let reservation = names.reserve(&candidate);
        assert!(names.taken(&candidate), "a starting session takes the name");
        assert!(!names.taken(&primary.join("program-002.mp4")));
        drop(reservation);
        assert!(!names.taken(&candidate));
    }

    #[test]
    fn planned_sessions_use_the_bare_name_and_the_fallback_once() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        let fallback = directory.path().join("fallback");
        fs::create_dir_all(&primary).unwrap();
        let names = SegmentNames::new(Some(fallback.clone()));
        let planned = plan(
            &primary.join("program-002.mp4"),
            &primary,
            &names,
            1,
            1,
            None,
        )
        .unwrap();
        assert_eq!(planned.config().name(), "program-002");
        assert_eq!(planned.config().roots(), [primary.clone(), fallback]);
        assert_eq!(
            planned.config().segment_path(0, 1).unwrap(),
            primary.join("program-002.mp4")
        );
        drop(planned);

        // A fallback that is the primary directory itself adds nothing.
        let same = SegmentNames::new(Some(primary.clone()));
        let planned = plan(&primary.join("program.mp4"), &primary, &same, 1, 1, None).unwrap();
        assert_eq!(planned.config().roots(), [primary]);
    }

    #[test]
    fn bitrate_estimates_prefer_configured_rates() {
        let rate = fm_types::FrameRate::new(30, 1).unwrap();
        // 4000 kbps video + 128 kbps audio, plus 1/32 overhead.
        assert_eq!(
            estimated_bitrate(1920, 1080, rate, 2, Some(4_000), Some(128)),
            4_128_000 + 4_128_000 / 32
        );
        // 1920x1080x30 at 0.15 bit per pixel, plus 64 kbps per channel.
        let payload = 1920 * 1080 * 30 * 150 / 1000 + 2 * 64_000;
        assert_eq!(
            estimated_bitrate(1920, 1080, rate, 2, None, None),
            payload + payload / 32
        );
    }

    #[test]
    fn preflight_refuses_only_when_every_measured_root_is_exhausted() {
        let thresholds = SpaceThresholds::default();
        let mut exhausted = |_: &Path| Ok(thresholds.reserve_bytes);
        let roots = [PathBuf::from("a"), PathBuf::from("b")];
        let preflight = preflight_recording_space(
            &mut exhausted,
            &roots,
            1_000_000,
            PREFLIGHT_MINIMUM,
            &thresholds,
        );
        assert!(preflight_refuses(&preflight));

        let mut mixed = |directory: &Path| {
            if directory == Path::new("a") {
                Ok(0)
            } else {
                Ok(u64::MAX / 2)
            }
        };
        let preflight = preflight_recording_space(
            &mut mixed,
            &roots,
            1_000_000,
            PREFLIGHT_MINIMUM,
            &thresholds,
        );
        assert!(!preflight_refuses(&preflight));
        let record = space_record(2, "program", 1_000_000, &preflight);
        assert!(
            record.starts_with(
                "FREEMIXD_RECORDER_SPACE\tv=1\tslot=2\tsession=program\tbitrate_bps=1000000\tminimum_s=300\tsufficient_root=1\troots=0:Exhausted:0,1:Normal:"
            ),
            "{record}"
        );

        let mut unknown = |_: &Path| Err(io::Error::from(io::ErrorKind::Unsupported));
        let preflight = preflight_recording_space(
            &mut unknown,
            &roots,
            1_000_000,
            PREFLIGHT_MINIMUM,
            &thresholds,
        );
        assert!(!preflight_refuses(&preflight), "unknown space never blocks");
    }

    #[test]
    fn derived_names_belong_to_their_stem_only() {
        assert!(derives_from("program", "program"));
        assert!(derives_from("program-002", "program"));
        assert!(!derives_from("program-02", "program"));
        assert!(!derives_from("program-0002", "program"));
        assert!(!derives_from("programs", "program"));
        assert!(!derives_from("other-002", "program"));
    }

    #[test]
    fn startup_recovery_repairs_open_sessions_and_skips_closed_ones() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        let fallback = directory.path().join("fallback");
        fs::create_dir_all(&primary).unwrap();
        let names = SegmentNames::new(Some(fallback.clone()));
        let output = primary.join("program.mp4");

        // A crashed session: open manifest, one segment holding garbage that
        // never became a fragmented MP4.
        let planned = plan(&output, &primary, &names, 1, 1, None).unwrap();
        let config = planned.config().clone();
        let storage: SessionStorage = FileStorage::new(disk::available_bytes);
        let mut session = RecordingSession::start(config.clone(), storage).unwrap();
        let mut opened = session.open_segment().unwrap();
        io::Write::write_all(&mut opened.sink, b"not an mp4").unwrap();
        drop(opened);
        drop(session);
        drop(planned);

        // A cleanly stopped sibling session is left alone.
        let stopped = session_config(
            "program-002",
            &primary,
            Some(&fallback),
            SegmentFormat::Fmp4,
        )
        .unwrap();
        let storage: SessionStorage = FileStorage::new(disk::available_bytes);
        RecordingSession::start(stopped.clone(), storage)
            .unwrap()
            .stop()
            .unwrap();
        // Another recorder's open session in the same directory is ignored.
        let other = session_config("other", &primary, None, SegmentFormat::Fmp4).unwrap();
        let storage: SessionStorage = FileStorage::new(disk::available_bytes);
        drop(RecordingSession::start(other, storage).unwrap());

        let records = recover_interrupted(
            &[RecoveryScope {
                slot: 1,
                output: &output,
            }],
            Some(&fallback),
        );
        assert_eq!(records.len(), 1, "{records:?}");
        assert!(
            records[0].starts_with(
                "FREEMIXD_RECORDING_RECOVERY\tv=1\tslot=1\tsession=program\tstatus=recovered\tprevious_state=Recording\tsegments=1\tintact=0\trepaired=0\tunrecoverable=1\tmissing=0\tkept_bytes=0\t"
            ),
            "{records:?}"
        );
        assert_eq!(
            newest_manifest(&config).unwrap().state,
            SessionState::Recovered
        );
        // A second startup finds nothing left to recover.
        assert_eq!(
            recover_interrupted(
                &[RecoveryScope {
                    slot: 1,
                    output: &output,
                }],
                Some(&fallback),
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn unused_sessions_leave_no_manifest_behind() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        let fallback = directory.path().join("fallback");
        fs::create_dir_all(&primary).unwrap();
        let names = SegmentNames::new(Some(fallback.clone()));
        let output = primary.join("program.mp4");
        let planned = plan(&output, &primary, &names, 1, 1, Some(1)).unwrap();
        let opened = open(planned).unwrap();
        assert!(primary.join("program.fmsession").exists());
        assert!(primary.join("program.wav.fmsession").exists());
        assert!(names.taken(&output), "the manifest takes the name");
        drop(opened.output);
        drop(opened.wav);
        discard_unused(&opened.status);
        for directory in [&primary, &fallback] {
            assert!(!directory.join("program.fmsession").exists());
            assert!(!directory.join("program.wav.fmsession").exists());
        }
        assert!(!names.taken(&output));
    }

    #[test]
    fn wav_sidecar_sessions_share_the_name_and_placement() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        let fallback = directory.path().join("fallback");
        fs::create_dir_all(&primary).unwrap();
        let names = SegmentNames::new(Some(fallback.clone()));
        let output = primary.join("program-002.mp4");
        let wav_bps = wav_bitrate(48_000, 2);
        assert_eq!(wav_bps, 1_536_000);
        let planned = plan(&output, &primary, &names, 1, 1_000, Some(wav_bps)).unwrap();
        assert_eq!(planned.bitrate_bits_per_second, 1_000 + wav_bps);
        let wav = planned.wav_config().unwrap();
        assert_eq!(wav.name(), "program-002");
        assert_eq!(wav.segment_format(), SegmentFormat::Wav);
        assert_eq!(wav.roots(), planned.config().roots());
        assert_eq!(wav.manifest_file_name(), "program-002.wav.fmsession");
        assert_eq!(
            wav.segment_path(0, 1).unwrap(),
            primary.join("program-002.wav")
        );
        assert_eq!(wav.segment_file_name(2), "program-002-0002.wav");
        drop(planned);
        assert!(
            plan(&output, &primary, &names, 1, 1, None)
                .unwrap()
                .wav_config()
                .is_none()
        );
    }

    #[test]
    fn names_are_taken_by_wav_sidecars_and_their_manifests() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        let fallback = directory.path().join("fallback");
        fs::create_dir_all(&primary).unwrap();
        fs::create_dir_all(&fallback).unwrap();
        let names = SegmentNames::new(Some(fallback.clone()));
        let candidate = primary.join("program.mp4");
        for root in [&primary, &fallback] {
            for file in ["program.wav", "program.wav.fmsession"] {
                fs::write(root.join(file), b"").unwrap();
                assert!(names.taken(&candidate), "{file} takes the name");
                fs::remove_file(root.join(file)).unwrap();
            }
        }
        // Failover fragments of another session never block a name.
        fs::write(primary.join("program-0002.wav"), b"").unwrap();
        assert!(!names.taken(&candidate));
    }

    #[test]
    fn a_refused_wav_session_discards_its_video_session() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        fs::create_dir_all(&primary).unwrap();
        let names = SegmentNames::new(None);
        let output = primary.join("program.mp4");
        let planned = plan(&output, &primary, &names, 1, 1, Some(1)).unwrap();
        // A stale WAV manifest of the same name refuses the WAV session.
        fs::write(primary.join("program.wav.fmsession"), b"stale").unwrap();
        let Err(error) = open(planned) else {
            panic!("a stale WAV manifest refuses the start");
        };
        assert!(error.starts_with("wav_session:"), "{error}");
        assert!(!primary.join("program.fmsession").exists());
        assert_eq!(
            fs::read(primary.join("program.wav.fmsession")).unwrap(),
            b"stale"
        );
    }

    #[test]
    fn startup_recovery_repairs_open_wav_sessions_with_the_wav_scanner() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        let fallback = directory.path().join("fallback");
        fs::create_dir_all(&primary).unwrap();
        fs::create_dir_all(&fallback).unwrap();
        let output = primary.join("program.mp4");
        let config =
            session_config("program", &primary, Some(&fallback), SegmentFormat::Wav).unwrap();
        let storage: SessionStorage = FileStorage::new(disk::available_bytes);
        let mut session = RecordingSession::start(config.clone(), storage).unwrap();
        let mut opened = session.open_segment().unwrap();
        // A streamed header with placeholder sizes, 3 stereo s16 frames, and
        // half a frame torn by the crash.
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&u32::MAX.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&48_000_u32.to_le_bytes());
        wav.extend_from_slice(&192_000_u32.to_le_bytes());
        wav.extend_from_slice(&4_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&u32::MAX.to_le_bytes());
        wav.extend_from_slice(&[1; 14]);
        io::Write::write_all(&mut opened.sink, &wav).unwrap();
        drop(opened);
        drop(session);
        assert!(primary.join("program.wav").exists());

        let records = recover_interrupted(
            &[RecoveryScope {
                slot: 1,
                output: &output,
            }],
            Some(&fallback),
        );
        assert_eq!(records.len(), 1, "{records:?}");
        assert!(
            records[0].starts_with(
                "FREEMIXD_RECORDING_WAV_RECOVERY\tv=1\tslot=1\tsession=program.wav\tstatus=recovered\tprevious_state=Recording\tsegments=1\tintact=0\trepaired=1\tunrecoverable=0\tmissing=0\tkept_bytes=56\tdiscarded_bytes=2\t"
            ),
            "{records:?}"
        );
        assert_eq!(fs::metadata(primary.join("program.wav")).unwrap().len(), 56);
        assert_eq!(
            newest_manifest(&config).unwrap().state,
            SessionState::Recovered
        );
        assert_eq!(
            recover_interrupted(
                &[RecoveryScope {
                    slot: 1,
                    output: &output,
                }],
                Some(&fallback),
            ),
            Vec::<String>::new(),
            "a second startup finds nothing left to recover"
        );
    }

    #[test]
    fn manifests_map_to_their_session_format() {
        assert_eq!(
            manifest_session("program.wav.fmsession", "program"),
            Some(("program".to_owned(), SegmentFormat::Wav))
        );
        assert_eq!(
            manifest_session("program-002.wav.fmsession", "program"),
            Some(("program-002".to_owned(), SegmentFormat::Wav))
        );
        assert_eq!(
            manifest_session("program.fmsession", "program"),
            Some(("program".to_owned(), SegmentFormat::Fmp4))
        );
        // A stem that itself ends in `.wav` keeps its video session.
        assert_eq!(
            manifest_session("show.wav.fmsession", "show.wav"),
            Some(("show.wav".to_owned(), SegmentFormat::Fmp4))
        );
        assert_eq!(manifest_session("other.wav.fmsession", "program"), None);
        assert_eq!(manifest_session("program.wav", "program"), None);
    }

    #[test]
    fn session_and_failover_records_are_path_free_and_stable() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        fs::create_dir_all(&primary).unwrap();
        let names = SegmentNames::new(None);
        let planned = plan(&primary.join("program.mp4"), &primary, &names, 2, 1, None).unwrap();
        let opened = open(planned).unwrap();
        drop(opened.output);
        let mut state = opened.status.get();
        let record = session_report(2, "program", &state);
        assert_eq!(
            record,
            "FREEMIXD_RECORDER_SESSION\tv=1\tslot=2\tsession=program\tstate=Stopped\tfinished=true\tsegments=0\tfragments=0\tbytes=0\tfailovers=0\tretries=0\trewritten_fragments=0\tactive_root=0\tdisabled_roots=none\terrors=0\terrors_dropped=0\tlast_error=none\tdiscarded_tail_bytes=0\tmanifest_stale=false\tfailure=none"
        );
        assert!(!record.contains(&*directory.path().to_string_lossy()));

        let mut watch = FailoverWatch::default();
        assert_eq!(watch.observe(2, "program", &state), None);
        let error = SessionErrorRecord {
            sequence: 1,
            segment: Some(1),
            root: 0,
            op: fm_record::FailureOp::Write,
            kind: fm_record::FailureKind::StorageFull,
            count: 1,
        };
        state.video.snapshot.disabled_roots = vec![0];
        state.video.snapshot.active_root = 1;
        state.video.snapshot.errors.push(error);
        assert_eq!(
            watch.observe(2, "program", &state).as_deref(),
            Some(
                "FREEMIXD_RECORDER_FAILOVER\tv=1\tslot=2\tsession=program\tstate=Stopped\tactive_root=1\tdisabled_roots=0\tcause=Write:StorageFull@0x1"
            )
        );
        assert_eq!(watch.observe(2, "program", &state), None, "reported once");

        // The primary had room again: one restore notice, and a second
        // failover from it is reported again.
        state.video.snapshot.disabled_roots.clear();
        state.video.snapshot.active_root = 0;
        assert_eq!(
            watch.observe(2, "program", &state).as_deref(),
            Some(
                "FREEMIXD_RECORDER_RESTORED\tv=1\tslot=2\tsession=program\tstate=Stopped\tactive_root=0\tdisabled_roots=none"
            )
        );
        assert_eq!(watch.observe(2, "program", &state), None, "reported once");
        state.video.snapshot.disabled_roots = vec![0];
        assert!(
            watch
                .observe(2, "program", &state)
                .is_some_and(|record| record.starts_with("FREEMIXD_RECORDER_FAILOVER\t"))
        );
    }

    #[test]
    fn remaining_time_records_fire_once_per_threshold_and_clear_with_margin() {
        // 8 Mbit/s is one million bytes per second above the reserve.
        let reserve = SpaceThresholds::default().reserve_bytes;
        let free = |seconds: u64| reserve + seconds * 1_000_000;
        let mut watch = RemainingTimeWatch::default();
        let mut sample =
            |seconds: u64| watch.observe_sample(1, "program", 1, free(seconds), 8_000_000);
        assert_eq!(sample(3_600), None, "plenty of time is not reported");
        assert_eq!(
            sample(1_800).as_deref(),
            Some(
                format!(
                    "FREEMIXD_RECORDER_REMAINING\tv=1\tslot=1\tsession=program\tstate=low\tthreshold_s=1800\tprevious_threshold_s=none\tremaining_s=1800\tactive_root=1\tfree_bytes={}\tbitrate_bps=8000000",
                    free(1_800)
                )
                .as_str()
            )
        );
        // Wobbling around the threshold, inside the clear margin, is quiet.
        assert_eq!(sample(1_790), None);
        assert_eq!(sample(1_850), None);
        assert_eq!(sample(1_795), None);
        let ten = sample(590).unwrap();
        assert!(ten.contains("\tstate=low\tthreshold_s=600\tprevious_threshold_s=1800\t"));
        let two = sample(100).unwrap();
        assert!(two.contains("\tthreshold_s=120\tprevious_threshold_s=600\tremaining_s=100\t"));
        assert_eq!(sample(170), None, "inside 2 minutes plus the margin");
        let eased = sample(200).unwrap();
        assert!(eased.contains("\tstate=low\tthreshold_s=600\tprevious_threshold_s=120\t"));
        let cleared = sample(7_200).unwrap();
        assert!(cleared.contains("\tstate=normal\tthreshold_s=none\tprevious_threshold_s=600\t"));
    }

    #[test]
    fn remaining_thresholds_parse_and_reject_bad_lists() {
        assert_eq!(
            parse_remaining_thresholds("30,10,2"),
            Some(vec![
                Duration::from_mins(30),
                Duration::from_mins(10),
                Duration::from_mins(2),
            ])
        );
        assert_eq!(
            parse_remaining_thresholds(" 60 , 15 "),
            Some(vec![Duration::from_hours(1), Duration::from_mins(15)])
        );
        for bad in [
            "",
            "0",
            "10,30",
            "30,30",
            "30,10,0",
            "30,x",
            "1441",
            "30,10,2,1,1,1,1,1,1",
        ] {
            assert_eq!(parse_remaining_thresholds(bad), None, "{bad}");
        }
    }

    #[test]
    fn remaining_time_watch_honours_configured_thresholds() {
        let reserve = SpaceThresholds::default().reserve_bytes;
        let free = |seconds: u64| reserve + seconds * 1_000_000;
        let mut watch = RemainingTimeWatch::with_thresholds(
            vec![Duration::from_mins(5)],
            Duration::from_mins(1),
        );
        assert_eq!(
            watch.observe_sample(1, "program", 1, free(600), 8_000_000),
            None
        );
        let low = watch
            .observe_sample(1, "program", 1, free(240), 8_000_000)
            .unwrap();
        assert!(low.contains("\tthreshold_s=300\t"), "{low}");
        assert_eq!(
            watch.observe_sample(1, "program", 1, free(330), 8_000_000),
            None,
            "inside the clear margin"
        );
        let cleared = watch
            .observe_sample(1, "program", 1, free(1_200), 8_000_000)
            .unwrap();
        assert!(
            cleared.contains("\tstate=normal\tthreshold_s=none\t"),
            "{cleared}"
        );
    }

    #[test]
    fn remaining_time_watch_samples_the_active_directory_at_a_bounded_rate() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        fs::create_dir_all(&primary).unwrap();
        let names = SegmentNames::new(None);
        // An absurd bitrate leaves under two minutes on any real disk.
        let planned = plan(
            &primary.join("program.mp4"),
            &primary,
            &names,
            1,
            u64::MAX / 2,
            None,
        )
        .unwrap();
        let opened = open(planned).unwrap();
        let mut watch = RemainingTimeWatch::default();
        let now = Instant::now();
        let record = watch.observe(1, &opened.status, now).unwrap();
        assert!(
            record.starts_with(
                "FREEMIXD_RECORDER_REMAINING\tv=1\tslot=1\tsession=program\tstate=low\tthreshold_s=120\tprevious_threshold_s=none\tremaining_s=0\tactive_root=0\t"
            ),
            "{record}"
        );
        assert!(!record.contains(&*directory.path().to_string_lossy()));
        assert_eq!(watch.observe(1, &opened.status, now), None);
        drop(opened.output);
    }

    #[test]
    fn startup_recovery_leaves_a_session_another_recorder_holds_alone() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        fs::create_dir_all(&primary).unwrap();
        let output = primary.join("program.mp4");
        let names = SegmentNames::new(None);
        let planned = plan(&output, &primary, &names, 1, 1, None).unwrap();
        let config = planned.config().clone();
        // A live session, as another daemon would hold it, with a segment
        // that is not yet a fragmented MP4.
        let storage: SessionStorage = FileStorage::new(disk::available_bytes);
        let mut session = RecordingSession::start(config.clone(), storage).unwrap();
        let mut opened = session.open_segment().unwrap();
        io::Write::write_all(&mut opened.sink, b"live bytes").unwrap();
        let scope = [RecoveryScope {
            slot: 1,
            output: &output,
        }];
        assert_eq!(
            recover_interrupted(&scope, None),
            ["FREEMIXD_RECORDING_RECOVERY\tv=1\tslot=1\tsession=program\tstatus=locked"]
        );
        assert_eq!(fs::read(&opened.path).unwrap(), b"live bytes");
        assert_eq!(
            newest_manifest(&config).unwrap().state,
            SessionState::Recording
        );
        // Once the holder is gone, the next startup recovers it.
        drop(opened);
        drop(session);
        drop(planned);
        let records = recover_interrupted(&scope, None);
        assert_eq!(records.len(), 1, "{records:?}");
        assert!(records[0].contains("\tstatus=recovered\t"), "{records:?}");
    }

    #[test]
    fn wav_session_and_failover_records_name_the_sidecar() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        fs::create_dir_all(&primary).unwrap();
        let names = SegmentNames::new(None);
        let planned = plan(
            &primary.join("program.mp4"),
            &primary,
            &names,
            2,
            1,
            Some(1),
        )
        .unwrap();
        let opened = open(planned).unwrap();
        drop(opened.output);
        drop(opened.wav);
        let mut state = opened.status.get();
        let report = session_report(2, "program", &state);
        let lines: Vec<&str> = report.lines().collect();
        assert_eq!(lines.len(), 2, "{report}");
        assert!(lines[0].starts_with("FREEMIXD_RECORDER_SESSION\tv=1\tslot=2\tsession=program\t"));
        assert_eq!(
            lines[1],
            "FREEMIXD_RECORDER_WAV_SESSION\tv=1\tslot=2\tsession=program.wav\tstate=Stopped\tfinished=true\tsegments=0\tframes=0\tdata_bytes=0\tbytes=0\tfailovers=0\tretries=0\trewritten_bytes=0\theader_patch_failures=0\tactive_root=0\tdisabled_roots=none\terrors=0\terrors_dropped=0\tlast_error=none\tdiscarded_tail_bytes=0\tmanifest_stale=false\tfailure=none"
        );
        assert!(!report.contains(&*directory.path().to_string_lossy()));

        let mut watch = FailoverWatch::default();
        assert_eq!(watch.observe(2, "program", &state), None);
        let wav = state.wav.as_mut().unwrap();
        wav.snapshot.disabled_roots = vec![0];
        wav.snapshot.active_root = 1;
        assert_eq!(
            watch.observe(2, "program", &state).as_deref(),
            Some(
                "FREEMIXD_RECORDER_FAILOVER\tv=1\tslot=2\tsession=program.wav\tstate=Stopped\tactive_root=1\tdisabled_roots=0\tcause=none"
            )
        );
        state.video.snapshot.disabled_roots = vec![0];
        let wav = state.wav.as_mut().unwrap();
        wav.snapshot.disabled_roots = vec![0, 1];
        let both = watch.observe(2, "program", &state).unwrap();
        assert_eq!(both.lines().count(), 2, "{both}");
        assert_eq!(watch.observe(2, "program", &state), None, "reported once");
    }
}
