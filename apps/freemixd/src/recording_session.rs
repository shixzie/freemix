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
//! Every record this module renders is path-free apart from the session name,
//! which is the operator's own file stem.

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use fm_codec_ffmpeg::record::{ResilientOutput, ResilientOutputState, ResilientOutputStatus};
use fm_record::{
    FileStorage, MAX_SESSION_NAME_BYTES, RecordingSession, ResilientWriterConfig,
    SESSION_MANIFEST_EXTENSION, SegmentOutcome, SessionConfig, SessionErrorRecord, SessionManifest,
    SessionRecovery, SessionState, SpaceLevel, SpacePreflight, SpaceThresholds,
    preflight_recording_space, recover_session,
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

    /// Whether `candidate` cannot name a new segment: its file exists in the
    /// primary or fallback directory, a session manifest of that name exists
    /// in either, or a session of that name is still starting.
    pub(crate) fn taken(&self, candidate: &Path) -> bool {
        if candidate.exists() {
            return true;
        }
        let Some(name) = session_name(candidate) else {
            return false;
        };
        let manifest = manifest_file_name(name);
        let primary = candidate
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if primary.join(&manifest).exists() {
            return true;
        }
        if let Some(fallback) = &self.fallback
            && (fallback.join(&manifest).exists()
                || candidate
                    .file_name()
                    .is_some_and(|file| fallback.join(file).exists()))
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

/// Builds the session configuration for segments under `primary`.
fn session_config(
    name: &str,
    primary: &Path,
    fallback: Option<&Path>,
) -> Result<SessionConfig, String> {
    let mut config = SessionConfig::new(name, primary)
        .map_err(|error| error.to_string())?
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
    slot: usize,
    bitrate_bits_per_second: u64,
    _reservation: Reservation,
}

#[cfg(test)]
impl PlannedSession {
    pub(crate) fn config(&self) -> &SessionConfig {
        &self.config
    }
}

/// Plans the session for `segment`, whose canonical parent is `primary`.
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
) -> Result<PlannedSession, String> {
    let name = session_name(segment).ok_or("recording output stem is not valid Unicode")?;
    let config = session_config(name, primary, names.fallback())?;
    Ok(PlannedSession {
        config,
        slot,
        bitrate_bits_per_second,
        _reservation: names.reserve(segment),
    })
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

/// Runs the free-space preflight, starts the session, and wraps it in the
/// fault-tolerant recorder output. Reports the preflight on stderr.
///
/// # Errors
///
/// Refuses when every root with a known free-space figure is exhausted, or
/// when the session cannot start (no usable directory, an existing manifest
/// of the same name, or a manifest that reaches no directory).
pub(crate) fn open(
    planned: PlannedSession,
) -> Result<
    (
        ResilientOutput<SessionStorage>,
        ResilientOutputStatus,
        SessionConfig,
    ),
    String,
> {
    let mut space_probe = probe;
    let preflight = preflight_recording_space(
        &mut space_probe,
        planned.config.roots(),
        planned.bitrate_bits_per_second,
        PREFLIGHT_MINIMUM,
        &SpaceThresholds::default(),
    );
    eprintln!(
        "{}",
        space_record(
            planned.slot,
            planned.config.name(),
            planned.bitrate_bits_per_second,
            &preflight
        )
    );
    if preflight_refuses(&preflight) {
        return Err("preflight:no recording directory has usable free space".to_owned());
    }
    let storage: SessionStorage = FileStorage::new(disk::available_bytes);
    let session = RecordingSession::start(planned.config.clone(), storage)
        .map_err(|error| format!("session:{error}"))?;
    let output = ResilientOutput::new(session, ResilientWriterConfig::default());
    let status = output.status();
    Ok((output, status, planned.config))
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

/// Removes the manifests of a session that never created a segment, so a
/// refused start leaves nothing behind and its name stays free.
pub(crate) fn discard_unused(config: &SessionConfig, status: &ResilientOutputStatus) {
    if !status.get().segments.is_empty() {
        return;
    }
    let manifest = config.manifest_file_name();
    for root in config.roots() {
        let _ = fs::remove_file(root.join(&manifest));
    }
}

/// Live failover detection for one open session.
#[derive(Debug, Default)]
pub(crate) struct FailoverWatch {
    disabled: usize,
}

impl FailoverWatch {
    /// Returns a `FREEMIXD_RECORDER_FAILOVER` record when a directory was
    /// disabled since the last observation.
    pub(crate) fn observe(
        &mut self,
        slot: usize,
        name: &str,
        state: &ResilientOutputState,
    ) -> Option<String> {
        let disabled = state.snapshot.disabled_roots.len();
        if disabled <= self.disabled {
            return None;
        }
        self.disabled = disabled;
        Some(format!(
            "FREEMIXD_RECORDER_FAILOVER\tv=1\tslot={slot}\tsession={}\tstate={:?}\tactive_root={}\tdisabled_roots={}\tcause={}",
            field(name),
            state.snapshot.state,
            state.snapshot.active_root,
            root_list(&state.snapshot.disabled_roots),
            error_field(state.snapshot.errors.last()),
        ))
    }
}

/// One `FREEMIXD_RECORDER_SESSION` record for a finished segment's session.
pub(crate) fn session_report(slot: usize, name: &str, state: &ResilientOutputState) -> String {
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
/// recovered is left alone. Sessions are found by manifest in the slot's
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
        let mut names: Vec<String> = [Some(primary.as_path()), fallback]
            .into_iter()
            .flatten()
            .filter_map(|directory| fs::read_dir(directory).ok())
            .flat_map(Iterator::flatten)
            .filter_map(|entry| {
                let file_name = entry.file_name().into_string().ok()?;
                let name = file_name
                    .strip_suffix(SESSION_MANIFEST_EXTENSION)?
                    .strip_suffix('.')?;
                derives_from(name, stem).then(|| name.to_owned())
            })
            .collect();
        names.sort();
        names.dedup();
        for name in names {
            if !seen.insert((primary.clone(), name.clone())) {
                continue;
            }
            let config = match session_config(&name, &primary, fallback) {
                Ok(config) => config,
                Err(error) => {
                    records.push(recovery_failure(scope.slot, &name, &error));
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
            match recover_session(&config) {
                Ok(recovery) => records.push(recovery_record(scope.slot, &name, &recovery)),
                Err(error) => {
                    records.push(recovery_failure(scope.slot, &name, &error.to_string()));
                }
            }
        }
    }
    records
}

fn recovery_failure(slot: usize, name: &str, error: &str) -> String {
    format!(
        "FREEMIXD_RECORDING_RECOVERY\tv=1\tslot={slot}\tsession={}\tstatus=failed\tfailure={}",
        field(name),
        field(error)
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
        let planned = plan(&primary.join("program-002.mp4"), &primary, &names, 1, 1).unwrap();
        assert_eq!(planned.config().name(), "program-002");
        assert_eq!(planned.config().roots(), [primary.clone(), fallback]);
        assert_eq!(
            planned.config().segment_path(0, 1).unwrap(),
            primary.join("program-002.mp4")
        );
        drop(planned);

        // A fallback that is the primary directory itself adds nothing.
        let same = SegmentNames::new(Some(primary.clone()));
        let planned = plan(&primary.join("program.mp4"), &primary, &same, 1, 1).unwrap();
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
        let planned = plan(&output, &primary, &names, 1, 1).unwrap();
        let config = planned.config().clone();
        let storage: SessionStorage = FileStorage::new(disk::available_bytes);
        let mut session = RecordingSession::start(config.clone(), storage).unwrap();
        let mut opened = session.open_segment().unwrap();
        io::Write::write_all(&mut opened.sink, b"not an mp4").unwrap();
        drop(opened);
        drop(session);
        drop(planned);

        // A cleanly stopped sibling session is left alone.
        let stopped = session_config("program-002", &primary, Some(&fallback)).unwrap();
        let storage: SessionStorage = FileStorage::new(disk::available_bytes);
        RecordingSession::start(stopped.clone(), storage)
            .unwrap()
            .stop()
            .unwrap();
        // Another recorder's open session in the same directory is ignored.
        let other = session_config("other", &primary, None).unwrap();
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
        let planned = plan(&output, &primary, &names, 1, 1).unwrap();
        let (sink, status, config) = open(planned).unwrap();
        assert!(names.taken(&output), "the manifest takes the name");
        drop(sink);
        discard_unused(&config, &status);
        assert!(!primary.join("program.fmsession").exists());
        assert!(!fallback.join("program.fmsession").exists());
        assert!(!names.taken(&output));
    }

    #[test]
    fn session_and_failover_records_are_path_free_and_stable() {
        let directory = tempfile::tempdir().unwrap();
        let primary = directory.path().join("primary");
        fs::create_dir_all(&primary).unwrap();
        let names = SegmentNames::new(None);
        let planned = plan(&primary.join("program.mp4"), &primary, &names, 2, 1).unwrap();
        let (sink, status, _) = open(planned).unwrap();
        drop(sink);
        let mut state = status.get();
        let record = session_report(2, "program", &state);
        assert_eq!(
            record,
            "FREEMIXD_RECORDER_SESSION\tv=1\tslot=2\tsession=program\tstate=Stopped\tfinished=true\tsegments=0\tfragments=0\tbytes=0\tfailovers=0\tretries=0\trewritten_fragments=0\tactive_root=0\tdisabled_roots=none\terrors=0\terrors_dropped=0\tlast_error=none\tdiscarded_tail_bytes=0\tmanifest_stale=false\tfailure=none"
        );
        assert!(!record.contains(&*directory.path().to_string_lossy()));

        let mut watch = FailoverWatch::default();
        assert_eq!(watch.observe(2, "program", &state), None);
        state.snapshot.disabled_roots = vec![0];
        state.snapshot.active_root = 1;
        state.snapshot.errors.push(SessionErrorRecord {
            sequence: 1,
            segment: Some(1),
            root: 0,
            op: fm_record::FailureOp::Write,
            kind: fm_record::FailureKind::StorageFull,
            count: 1,
        });
        assert_eq!(
            watch.observe(2, "program", &state).as_deref(),
            Some(
                "FREEMIXD_RECORDER_FAILOVER\tv=1\tslot=2\tsession=program\tstate=Stopped\tactive_root=1\tdisabled_roots=0\tcause=Write:StorageFull@0x1"
            )
        );
        assert_eq!(watch.observe(2, "program", &state), None, "reported once");
    }
}
