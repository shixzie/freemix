//! Crash-safe recording sessions over fragmented MP4 segment files.
//!
//! A [`RecordingSession`] owns the naming, placement, and bookkeeping of one
//! recording's segment files across a primary directory and up to three
//! fallback directories. A small text manifest, `<name>.fmsession`, is
//! rewritten atomically (temporary file, `fsync`, rename, directory `fsync`)
//! into every usable root on each segment open, close, and failure, and
//! carries a generation counter and a CRC32 so recovery can pick the newest
//! intact copy even when one root filled up or vanished.
//!
//! The manifest records a segment as `open` before its file is created, so a
//! crash at any point leaves a manifest that either names the segment or
//! predates it; recovery additionally adopts orphan segment files that match
//! the session's naming. [`recover_session`] then validates every segment with
//! the fragmented MP4 scanner, truncates torn tails to their last complete
//! fragment, and reports exactly what was kept and lost.
//!
//! Write failures are classified ([`FailureKind`]) and fed through a bounded
//! policy: transient errors retry on the same directory a bounded number of
//! times, while full, read-only, missing, or forbidden directories are
//! disabled and the session moves to the next usable fallback. When no
//! directory remains the session enters [`SessionState::Failed`] and every
//! caller-facing operation reports [`SessionError::Exhausted`]; the session
//! never stops silently. Errors are kept in a bounded, de-duplicated ring.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use crate::{
    fmp4::{Fmp4Integrity, Fmp4Scan, repair_fmp4_file},
    format::{crc32, sync_directory_io},
};

/// Extension of the session manifest file.
pub const SESSION_MANIFEST_EXTENSION: &str = "fmsession";
/// Extension of every segment file.
pub const SESSION_SEGMENT_EXTENSION: &str = "mp4";
/// Primary plus fallback directories.
pub const MAX_SESSION_ROOTS: usize = 4;
/// Segment indexes run from 1 through this value (four digits).
pub const MAX_SESSION_SEGMENTS: u32 = 9999;
/// Upper bound for the retained error ring.
pub const MAX_SESSION_ERRORS: usize = 64;
/// Longest session name in bytes.
pub const MAX_SESSION_NAME_BYTES: usize = 64;
const MAX_ATTEMPTS_PER_ROOT: u32 = 16;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MANIFEST_MAGIC: &str = "freemix-recording-session 1";

/// Validated session placement and failure policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    name: String,
    roots: Vec<PathBuf>,
    max_segments: u32,
    max_errors: usize,
    attempts_per_root: u32,
    min_free_bytes: u64,
    bare_first_segment: bool,
}

impl SessionConfig {
    /// Creates a configuration with one primary directory.
    ///
    /// `name` must be 1 to 64 ASCII letters, digits, `-`, `_`, or `.`, and
    /// must not start with `.`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::InvalidConfig`] for an invalid name.
    pub fn new(name: &str, primary: impl Into<PathBuf>) -> Result<Self, SessionError> {
        let valid = !name.is_empty()
            && name.len() <= MAX_SESSION_NAME_BYTES
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        if !valid {
            return Err(SessionError::InvalidConfig("invalid session name"));
        }
        Ok(Self {
            name: name.to_owned(),
            roots: vec![primary.into()],
            max_segments: MAX_SESSION_SEGMENTS,
            max_errors: 32,
            attempts_per_root: 3,
            min_free_bytes: 0,
            bare_first_segment: false,
        })
    }

    /// Adds a fallback directory, tried in insertion order.
    ///
    /// # Errors
    ///
    /// Refuses more than [`MAX_SESSION_ROOTS`] roots or a duplicate root.
    pub fn with_fallback(mut self, root: impl Into<PathBuf>) -> Result<Self, SessionError> {
        let root = root.into();
        if self.roots.len() == MAX_SESSION_ROOTS {
            return Err(SessionError::InvalidConfig("too many recording roots"));
        }
        if self.roots.contains(&root) {
            return Err(SessionError::InvalidConfig("duplicate recording root"));
        }
        self.roots.push(root);
        Ok(self)
    }

    /// # Errors
    ///
    /// Refuses zero or more than [`MAX_SESSION_SEGMENTS`].
    pub fn with_max_segments(mut self, max_segments: u32) -> Result<Self, SessionError> {
        if !(1..=MAX_SESSION_SEGMENTS).contains(&max_segments) {
            return Err(SessionError::InvalidConfig("segment limit out of range"));
        }
        self.max_segments = max_segments;
        Ok(self)
    }

    /// # Errors
    ///
    /// Refuses zero or more than [`MAX_SESSION_ERRORS`].
    pub fn with_max_errors(mut self, max_errors: usize) -> Result<Self, SessionError> {
        if !(1..=MAX_SESSION_ERRORS).contains(&max_errors) {
            return Err(SessionError::InvalidConfig("error ring size out of range"));
        }
        self.max_errors = max_errors;
        Ok(self)
    }

    /// Consecutive transient failures tolerated on one root before it is
    /// disabled.
    ///
    /// # Errors
    ///
    /// Refuses zero or more than 16.
    pub fn with_attempts_per_root(mut self, attempts: u32) -> Result<Self, SessionError> {
        if !(1..=MAX_ATTEMPTS_PER_ROOT).contains(&attempts) {
            return Err(SessionError::InvalidConfig(
                "attempts per root out of range",
            ));
        }
        self.attempts_per_root = attempts;
        Ok(self)
    }

    /// A root whose probed free space is below this value is not selected.
    /// Unknown free space never blocks a root.
    #[must_use]
    pub fn with_min_free_bytes(mut self, bytes: u64) -> Self {
        self.min_free_bytes = bytes;
        self
    }

    /// Names segment 1 `<name>.mp4` instead of `<name>-0001.mp4`, so a
    /// session that never fails over or rotates leaves exactly the file the
    /// operator named. Later segments keep the four-digit form. Recovery must
    /// use the same setting to find segment 1.
    #[must_use]
    pub fn with_bare_first_segment(mut self) -> Self {
        self.bare_first_segment = true;
        self
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    #[must_use]
    pub fn max_segments(&self) -> u32 {
        self.max_segments
    }

    /// File name of segment `index`: `<name>-NNNN.mp4`, or `<name>.mp4` for
    /// segment 1 with [`Self::with_bare_first_segment`].
    #[must_use]
    pub fn segment_file_name(&self, index: u32) -> String {
        if self.bare_first_segment && index == 1 {
            return format!("{}.{SESSION_SEGMENT_EXTENSION}", self.name);
        }
        format!("{}-{index:04}.{SESSION_SEGMENT_EXTENSION}", self.name)
    }

    /// File name of the manifest: `<name>.fmsession`.
    #[must_use]
    pub fn manifest_file_name(&self) -> String {
        format!("{}.{SESSION_MANIFEST_EXTENSION}", self.name)
    }

    /// Path of segment `index` under root `root`, if that root exists.
    #[must_use]
    pub fn segment_path(&self, root: usize, index: u32) -> Option<PathBuf> {
        self.roots
            .get(root)
            .map(|directory| directory.join(self.segment_file_name(index)))
    }

    fn parse_segment_file_name(&self, file_name: &str) -> Option<u32> {
        let rest = file_name.strip_prefix(self.name.as_str())?;
        if self.bare_first_segment && rest == format!(".{SESSION_SEGMENT_EXTENSION}") {
            return Some(1);
        }
        let digits = rest
            .strip_prefix('-')?
            .strip_suffix(SESSION_SEGMENT_EXTENSION)?
            .strip_suffix('.')?;
        if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let minimum = if self.bare_first_segment { 2 } else { 1 };
        digits.parse().ok().filter(|index| *index >= minimum)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Recording,
    Stopped,
    /// Every root failed; the recording stopped and the reason is in the
    /// error ring.
    Failed,
    /// A crash recovery rewrote the manifest.
    Recovered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentState {
    /// Being written, or the writer crashed.
    Open,
    Closed,
    /// A write failure ended the segment; `bytes` is the last complete
    /// fragment boundary the writer knew of.
    Failed,
    /// Validated, and repaired if needed, by recovery.
    Recovered,
    /// Recovery found nothing playable.
    Lost,
}

/// One segment entry in the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentEntry {
    pub index: u32,
    pub root: usize,
    pub state: SegmentState,
    /// Bytes known complete when the entry was last updated.
    pub bytes: u64,
}

/// Classified cause of an I/O failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    StorageFull,
    QuotaExceeded,
    ReadOnly,
    PermissionDenied,
    FileTooLarge,
    NotFound,
    AlreadyExists,
    Interrupted,
    Other,
}

impl FailureKind {
    /// Classifies an I/O error, including raw `ENOSPC`, `EDQUOT`, `EROFS`, and
    /// `EFBIG` codes on Unix.
    #[must_use]
    pub fn classify(error: &io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::StorageFull => Self::StorageFull,
            io::ErrorKind::QuotaExceeded => Self::QuotaExceeded,
            io::ErrorKind::ReadOnlyFilesystem => Self::ReadOnly,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::FileTooLarge => Self::FileTooLarge,
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::AlreadyExists => Self::AlreadyExists,
            io::ErrorKind::Interrupted | io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                Self::Interrupted
            }
            _ => Self::classify_raw(error.raw_os_error()),
        }
    }

    #[cfg(unix)]
    fn classify_raw(code: Option<i32>) -> Self {
        match code {
            Some(28) => Self::StorageFull,
            Some(30) => Self::ReadOnly,
            Some(27) => Self::FileTooLarge,
            #[cfg(target_os = "linux")]
            Some(122) => Self::QuotaExceeded,
            _ => Self::Other,
        }
    }

    #[cfg(not(unix))]
    fn classify_raw(_code: Option<i32>) -> Self {
        Self::Other
    }

    /// Whether the failure makes the whole root unusable for this session.
    #[must_use]
    pub fn disables_root(self) -> bool {
        matches!(
            self,
            Self::StorageFull
                | Self::QuotaExceeded
                | Self::ReadOnly
                | Self::PermissionDenied
                | Self::NotFound
        )
    }

    fn label(self) -> &'static str {
        match self {
            Self::StorageFull => "storage-full",
            Self::QuotaExceeded => "quota-exceeded",
            Self::ReadOnly => "read-only",
            Self::PermissionDenied => "permission-denied",
            Self::FileTooLarge => "file-too-large",
            Self::NotFound => "not-found",
            Self::AlreadyExists => "already-exists",
            Self::Interrupted => "interrupted",
            Self::Other => "other",
        }
    }

    fn from_label(label: &str) -> Option<Self> {
        [
            Self::StorageFull,
            Self::QuotaExceeded,
            Self::ReadOnly,
            Self::PermissionDenied,
            Self::FileTooLarge,
            Self::NotFound,
            Self::AlreadyExists,
            Self::Interrupted,
            Self::Other,
        ]
        .into_iter()
        .find(|kind| kind.label() == label)
    }
}

/// Operation that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureOp {
    CreateDirectory,
    CreateSegment,
    Write,
    Sync,
    Truncate,
    Manifest,
    /// Free space fell below the configured minimum.
    Preflight,
}

impl FailureOp {
    fn label(self) -> &'static str {
        match self {
            Self::CreateDirectory => "create-directory",
            Self::CreateSegment => "create-segment",
            Self::Write => "write",
            Self::Sync => "sync",
            Self::Truncate => "truncate",
            Self::Manifest => "manifest",
            Self::Preflight => "preflight",
        }
    }

    fn from_label(label: &str) -> Option<Self> {
        [
            Self::CreateDirectory,
            Self::CreateSegment,
            Self::Write,
            Self::Sync,
            Self::Truncate,
            Self::Manifest,
            Self::Preflight,
        ]
        .into_iter()
        .find(|op| op.label() == label)
    }
}

/// One retained failure. Consecutive identical failures are folded into one
/// record with a repeat count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionErrorRecord {
    pub sequence: u64,
    pub segment: Option<u32>,
    pub root: usize,
    pub op: FailureOp,
    pub kind: FailureKind,
    pub count: u32,
}

/// Persistent session manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionManifest {
    pub name: String,
    pub generation: u64,
    pub state: SessionState,
    pub segments: Vec<SegmentEntry>,
    pub errors: Vec<SessionErrorRecord>,
    pub errors_dropped: u64,
}

impl SessionManifest {
    /// Renders the manifest as deterministic text ending with a CRC32 line.
    #[must_use]
    pub fn render(&self) -> String {
        use fmt::Write as _;
        let mut body = String::new();
        let _ = writeln!(body, "{MANIFEST_MAGIC}");
        let _ = writeln!(body, "name {}", self.name);
        let _ = writeln!(body, "generation {}", self.generation);
        let _ = writeln!(body, "state {}", session_state_label(self.state));
        for segment in &self.segments {
            let _ = writeln!(
                body,
                "segment {} {} {} {}",
                segment.index,
                segment.root,
                segment_state_label(segment.state),
                segment.bytes
            );
        }
        for error in &self.errors {
            let segment = error
                .segment
                .map_or_else(|| "-".to_owned(), |index| index.to_string());
            let _ = writeln!(
                body,
                "error {} {segment} {} {} {} {}",
                error.sequence,
                error.root,
                error.op.label(),
                error.kind.label(),
                error.count
            );
        }
        let _ = writeln!(body, "errors-dropped {}", self.errors_dropped);
        let checksum = crc32(body.as_bytes());
        let _ = writeln!(body, "crc32 {checksum:08x}");
        body
    }

    /// Parses a rendered manifest, refusing any structural, bound, or
    /// checksum violation.
    ///
    /// # Errors
    ///
    /// Returns a static reason for the first violation.
    #[allow(clippy::too_many_lines)]
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let crc_start = text
            .trim_end_matches('\n')
            .rfind('\n')
            .map_or(0, |position| position + 1);
        let (body, crc_line) = text.split_at(crc_start);
        let expected = crc_line
            .strip_prefix("crc32 ")
            .and_then(|value| value.strip_suffix('\n'))
            .filter(|value| value.len() == 8)
            .and_then(|value| u32::from_str_radix(value, 16).ok())
            .ok_or("missing checksum line")?;
        if crc32(body.as_bytes()) != expected {
            return Err("checksum mismatch");
        }
        let mut lines = body.lines();
        if lines.next() != Some(MANIFEST_MAGIC) {
            return Err("unknown manifest header");
        }
        let name = field(lines.next(), "name")?.to_owned();
        SessionConfig::new(&name, PathBuf::new()).map_err(|_| "invalid session name")?;
        let generation = field(lines.next(), "generation")?
            .parse()
            .map_err(|_| "invalid generation")?;
        let state = parse_session_state(field(lines.next(), "state")?)?;
        let mut segments: Vec<SegmentEntry> = Vec::new();
        let mut errors: Vec<SessionErrorRecord> = Vec::new();
        let mut errors_dropped = None;
        for line in lines {
            if errors_dropped.is_some() {
                return Err("data after errors-dropped");
            }
            let mut words = line.split(' ');
            match words.next() {
                Some("segment") => {
                    if !errors.is_empty() {
                        return Err("segment after error records");
                    }
                    let entry = SegmentEntry {
                        index: number(words.next())?,
                        root: number(words.next())?,
                        state: parse_segment_state(words.next().ok_or("short segment line")?)?,
                        bytes: number(words.next())?,
                    };
                    if words.next().is_some() {
                        return Err("long segment line");
                    }
                    if !(1..=MAX_SESSION_SEGMENTS).contains(&entry.index)
                        || entry.root >= MAX_SESSION_ROOTS
                        || segments
                            .last()
                            .is_some_and(|last| last.index >= entry.index)
                    {
                        return Err("invalid segment entry");
                    }
                    segments.push(entry);
                }
                Some("error") => {
                    let sequence = number(words.next())?;
                    let segment = match words.next() {
                        Some("-") => None,
                        other => Some(number(other)?),
                    };
                    let record = SessionErrorRecord {
                        sequence,
                        segment,
                        root: number(words.next())?,
                        op: words
                            .next()
                            .and_then(FailureOp::from_label)
                            .ok_or("invalid error op")?,
                        kind: words
                            .next()
                            .and_then(FailureKind::from_label)
                            .ok_or("invalid error kind")?,
                        count: number(words.next())?,
                    };
                    if words.next().is_some()
                        || record.root >= MAX_SESSION_ROOTS
                        || record.count == 0
                        || errors.len() == MAX_SESSION_ERRORS
                        || errors.last().is_some_and(|last| last.sequence >= sequence)
                    {
                        return Err("invalid error record");
                    }
                    errors.push(record);
                }
                Some("errors-dropped") => {
                    errors_dropped = Some(number(words.next())?);
                    if words.next().is_some() {
                        return Err("long errors-dropped line");
                    }
                }
                _ => return Err("unknown manifest line"),
            }
        }
        Ok(Self {
            name,
            generation,
            state,
            segments,
            errors,
            errors_dropped: errors_dropped.ok_or("missing errors-dropped line")?,
        })
    }
}

fn field<'a>(line: Option<&'a str>, key: &str) -> Result<&'a str, &'static str> {
    line.and_then(|line| line.strip_prefix(key))
        .and_then(|rest| rest.strip_prefix(' '))
        .ok_or("missing manifest field")
}

fn number<T: std::str::FromStr>(word: Option<&str>) -> Result<T, &'static str> {
    word.filter(|word| {
        !word.is_empty()
            && word.bytes().all(|byte| byte.is_ascii_digit())
            && (word.len() == 1 || !word.starts_with('0'))
    })
    .and_then(|word| word.parse().ok())
    .ok_or("invalid number")
}

fn session_state_label(state: SessionState) -> &'static str {
    match state {
        SessionState::Recording => "recording",
        SessionState::Stopped => "stopped",
        SessionState::Failed => "failed",
        SessionState::Recovered => "recovered",
    }
}

fn parse_session_state(label: &str) -> Result<SessionState, &'static str> {
    match label {
        "recording" => Ok(SessionState::Recording),
        "stopped" => Ok(SessionState::Stopped),
        "failed" => Ok(SessionState::Failed),
        "recovered" => Ok(SessionState::Recovered),
        _ => Err("invalid session state"),
    }
}

fn segment_state_label(state: SegmentState) -> &'static str {
    match state {
        SegmentState::Open => "open",
        SegmentState::Closed => "closed",
        SegmentState::Failed => "failed",
        SegmentState::Recovered => "recovered",
        SegmentState::Lost => "lost",
    }
}

fn parse_segment_state(label: &str) -> Result<SegmentState, &'static str> {
    match label {
        "open" => Ok(SegmentState::Open),
        "closed" => Ok(SegmentState::Closed),
        "failed" => Ok(SegmentState::Failed),
        "recovered" => Ok(SegmentState::Recovered),
        "lost" => Ok(SegmentState::Lost),
        _ => Err("invalid segment state"),
    }
}

#[derive(Debug)]
pub enum SessionError {
    InvalidConfig(&'static str),
    /// A manifest with this session name already exists under `root`;
    /// recover it or choose another name.
    ManifestExists {
        root: usize,
    },
    /// No configured root is usable at start.
    NoUsableRoot,
    /// Every root failed; the session is in [`SessionState::Failed`].
    Exhausted {
        last: Option<SessionErrorRecord>,
    },
    /// The configured segment limit was reached.
    SegmentsExhausted,
    InvalidState(&'static str),
    /// The manifest could not be written to any root.
    ManifestUnwritable(io::ErrorKind),
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(reason) => write!(formatter, "invalid session config: {reason}"),
            Self::ManifestExists { root } => {
                write!(
                    formatter,
                    "a session manifest already exists in root {root}"
                )
            }
            Self::NoUsableRoot => formatter.write_str("no recording root is usable"),
            Self::Exhausted { last } => match last {
                Some(record) => write!(
                    formatter,
                    "every recording root failed; last failure: {} {} on root {}",
                    record.op.label(),
                    record.kind.label(),
                    record.root
                ),
                None => formatter.write_str("every recording root failed"),
            },
            Self::SegmentsExhausted => formatter.write_str("segment limit reached"),
            Self::InvalidState(reason) => write!(formatter, "invalid session state: {reason}"),
            Self::ManifestUnwritable(kind) => {
                write!(formatter, "session manifest is unwritable: {kind}")
            }
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Byte sink for one segment file.
pub trait SegmentSink {
    /// # Errors
    ///
    /// Any I/O error; the caller treats it as a segment failure.
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// # Errors
    ///
    /// Any I/O error; the caller treats it as a segment failure.
    fn sync_data(&mut self) -> io::Result<()>;
    /// Truncates the segment to `length` bytes.
    ///
    /// # Errors
    ///
    /// Any I/O error; recovery repairs the file later.
    fn truncate(&mut self, length: u64) -> io::Result<()>;
}

impl SegmentSink for File {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        Write::write_all(self, bytes)
    }

    fn sync_data(&mut self) -> io::Result<()> {
        File::sync_data(self)
    }

    fn truncate(&mut self, length: u64) -> io::Result<()> {
        use std::io::Seek as _;
        self.set_len(length)?;
        self.seek(io::SeekFrom::Start(length))?;
        File::sync_data(self)
    }
}

/// Free-space query for a directory.
pub trait SpaceProbe {
    /// # Errors
    ///
    /// Any I/O error; callers treat the space as unknown.
    fn available_bytes(&mut self, directory: &Path) -> io::Result<u64>;
}

impl<F: FnMut(&Path) -> io::Result<u64>> SpaceProbe for F {
    fn available_bytes(&mut self, directory: &Path) -> io::Result<u64> {
        self(directory)
    }
}

/// Space probe for platforms or callers without a free-space query.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnknownSpace;

impl SpaceProbe for UnknownSpace {
    fn available_bytes(&mut self, _directory: &Path) -> io::Result<u64> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// Creates segment sinks and probes free space.
pub trait SegmentStorage: SpaceProbe {
    type Sink: SegmentSink;

    /// Creates a new segment file; must fail if `path` exists.
    ///
    /// # Errors
    ///
    /// Any I/O error.
    fn create_segment(&mut self, path: &Path) -> io::Result<Self::Sink>;
}

/// Filesystem segment storage. Segments are created exclusively and their
/// directory entry is synced before the sink is returned.
#[derive(Debug)]
pub struct FileStorage<P> {
    probe: P,
}

impl<P: SpaceProbe> FileStorage<P> {
    pub fn new(probe: P) -> Self {
        Self { probe }
    }
}

impl<P: SpaceProbe> SpaceProbe for FileStorage<P> {
    fn available_bytes(&mut self, directory: &Path) -> io::Result<u64> {
        self.probe.available_bytes(directory)
    }
}

impl<P: SpaceProbe> SegmentStorage for FileStorage<P> {
    type Sink = File;

    fn create_segment(&mut self, path: &Path) -> io::Result<File> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        if let Some(parent) = path.parent() {
            sync_directory_io(parent)?;
        }
        Ok(file)
    }
}

/// What the writer should do after a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum FailureDecision {
    /// Open a new segment on the same root.
    Retry { root: usize },
    /// The root was disabled; open a new segment on root `to`.
    Fallback { from: usize, to: usize },
    /// No usable root remains; the session is failed.
    Exhausted,
}

/// A newly created segment.
#[derive(Debug)]
pub struct OpenSegment<K> {
    pub index: u32,
    pub root: usize,
    pub path: PathBuf,
    pub sink: K,
}

/// Point-in-time session status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub state: SessionState,
    pub active_root: usize,
    pub disabled_roots: Vec<usize>,
    pub open_segment: Option<u32>,
    pub segments: usize,
    pub errors: Vec<SessionErrorRecord>,
    pub errors_dropped: u64,
    pub manifest_generation: u64,
    /// The last manifest update reached no root.
    pub manifest_stale: bool,
}

/// One recording's segment placement, manifest, and failure policy.
#[derive(Debug)]
pub struct RecordingSession<S> {
    config: SessionConfig,
    storage: S,
    manifest: SessionManifest,
    active_root: usize,
    root_failures: Vec<u32>,
    root_disabled: Vec<bool>,
    open: Option<usize>,
    next_index: u32,
    next_error_sequence: u64,
    manifest_stale: bool,
}

impl<S: SegmentStorage> RecordingSession<S> {
    /// Starts a session: creates missing root directories, selects the first
    /// usable root, and writes the first manifest generation.
    ///
    /// # Errors
    ///
    /// Refuses when a manifest of the same name exists in any root, when no
    /// root is usable, or when the manifest reaches no root.
    pub fn start(config: SessionConfig, storage: S) -> Result<Self, SessionError> {
        let manifest_name = config.manifest_file_name();
        for (root, directory) in config.roots.iter().enumerate() {
            let path = directory.join(&manifest_name);
            if path
                .try_exists()
                .map_err(|source| SessionError::Io { path, source })?
            {
                return Err(SessionError::ManifestExists { root });
            }
        }
        let roots = config.roots.len();
        let mut session = Self {
            manifest: SessionManifest {
                name: config.name.clone(),
                generation: 0,
                state: SessionState::Recording,
                segments: Vec::new(),
                errors: Vec::new(),
                errors_dropped: 0,
            },
            config,
            storage,
            active_root: 0,
            root_failures: vec![0; roots],
            root_disabled: vec![false; roots],
            open: None,
            next_index: 1,
            next_error_sequence: 1,
            manifest_stale: false,
        };
        for root in 0..roots {
            if let Err(error) = fs::create_dir_all(&session.config.roots[root]) {
                session.push_error(None, root, FailureOp::CreateDirectory, &error);
                session.root_disabled[root] = true;
            }
        }
        session.active_root = session
            .first_usable_root()
            .ok_or(SessionError::NoUsableRoot)?;
        if let Err(kind) = session.persist() {
            return Err(SessionError::ManifestUnwritable(kind));
        }
        Ok(session)
    }

    #[must_use]
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    #[must_use]
    pub fn manifest(&self) -> &SessionManifest {
        &self.manifest
    }

    pub fn storage_mut(&mut self) -> &mut S {
        &mut self.storage
    }

    #[must_use]
    pub fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            state: self.manifest.state,
            active_root: self.active_root,
            disabled_roots: (0..self.root_disabled.len())
                .filter(|root| self.root_disabled[*root])
                .collect(),
            open_segment: self
                .open
                .map(|position| self.manifest.segments[position].index),
            segments: self.manifest.segments.len(),
            errors: self.manifest.errors.clone(),
            errors_dropped: self.manifest.errors_dropped,
            manifest_generation: self.manifest.generation,
            manifest_stale: self.manifest_stale,
        }
    }

    /// Opens the next segment on the active root. A creation failure goes
    /// through the failure policy and the next candidate is tried, within a
    /// bounded number of attempts.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Exhausted`] when every root failed,
    /// [`SessionError::SegmentsExhausted`] at the segment limit, and
    /// [`SessionError::InvalidState`] when not recording or a segment is open.
    pub fn open_segment(&mut self) -> Result<OpenSegment<S::Sink>, SessionError> {
        self.ensure_recording()?;
        if self.open.is_some() {
            return Err(SessionError::InvalidState("a segment is already open"));
        }
        let budget = self.config.roots.len() * (self.config.attempts_per_root as usize + 1) + 1;
        for _ in 0..budget {
            if self.next_index > self.config.max_segments {
                self.fail();
                return Err(SessionError::SegmentsExhausted);
            }
            let index = self.next_index;
            self.next_index += 1;
            let root = self.active_root;
            let path = self.config.roots[root].join(self.config.segment_file_name(index));
            self.manifest.segments.push(SegmentEntry {
                index,
                root,
                state: SegmentState::Open,
                bytes: 0,
            });
            let position = self.manifest.segments.len() - 1;
            self.persist_or_record();
            match self.storage.create_segment(&path) {
                Ok(sink) => {
                    self.open = Some(position);
                    return Ok(OpenSegment {
                        index,
                        root,
                        path,
                        sink,
                    });
                }
                Err(error) => {
                    self.manifest.segments[position].state = SegmentState::Failed;
                    let decision =
                        self.note_failure(Some(index), root, FailureOp::CreateSegment, &error);
                    self.persist_or_record();
                    if decision == FailureDecision::Exhausted {
                        return Err(self.exhausted());
                    }
                }
            }
        }
        self.fail();
        Err(self.exhausted())
    }

    /// Ends the open segment after a write, sync, or truncate failure.
    /// `valid_bytes` is the last complete fragment boundary the writer knows.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::InvalidState`] when no segment is open.
    pub fn record_failure(
        &mut self,
        op: FailureOp,
        error: &io::Error,
        valid_bytes: u64,
    ) -> Result<FailureDecision, SessionError> {
        let position = self
            .open
            .take()
            .ok_or(SessionError::InvalidState("no segment is open"))?;
        let entry = &mut self.manifest.segments[position];
        entry.state = SegmentState::Failed;
        entry.bytes = valid_bytes;
        let (index, root) = (entry.index, entry.root);
        let decision = self.note_failure(Some(index), root, op, error);
        self.persist_or_record();
        Ok(decision)
    }

    /// Records a failure that did not end the open segment, such as a failed
    /// best-effort truncate.
    pub fn record_incident(&mut self, op: FailureOp, error: &io::Error) {
        let segment = self
            .open
            .map(|position| self.manifest.segments[position].index);
        self.push_error(segment, self.active_root, op, error);
    }

    /// Resets the active root's consecutive-failure count after data landed
    /// successfully.
    pub fn mark_healthy(&mut self) {
        self.root_failures[self.active_root] = 0;
    }

    /// Updates the open segment's known-complete byte count in memory. It is
    /// persisted with the next manifest update.
    pub fn note_progress(&mut self, bytes: u64) {
        if let Some(position) = self.open {
            self.manifest.segments[position].bytes = bytes;
        }
    }

    /// Closes the open segment cleanly.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::InvalidState`] when no segment is open. A
    /// manifest that reaches no root is recorded and reported through
    /// [`SessionSnapshot::manifest_stale`], not as an error.
    pub fn close_segment(&mut self, bytes: u64) -> Result<(), SessionError> {
        let position = self
            .open
            .take()
            .ok_or(SessionError::InvalidState("no segment is open"))?;
        let entry = &mut self.manifest.segments[position];
        entry.state = SegmentState::Closed;
        entry.bytes = bytes;
        self.persist_or_record();
        Ok(())
    }

    /// Stops the session and returns the final manifest.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::InvalidState`] while a segment is open.
    pub fn stop(mut self) -> Result<SessionSnapshot, SessionError> {
        if self.open.is_some() {
            return Err(SessionError::InvalidState("close the open segment first"));
        }
        if self.manifest.state == SessionState::Recording {
            self.manifest.state = SessionState::Stopped;
        }
        self.persist_or_record();
        Ok(self.snapshot())
    }

    fn ensure_recording(&self) -> Result<(), SessionError> {
        match self.manifest.state {
            SessionState::Recording => Ok(()),
            SessionState::Failed => Err(self.exhausted()),
            _ => Err(SessionError::InvalidState("session is not recording")),
        }
    }

    fn exhausted(&self) -> SessionError {
        SessionError::Exhausted {
            last: self.manifest.errors.last().copied(),
        }
    }

    fn fail(&mut self) {
        self.manifest.state = SessionState::Failed;
        self.persist_or_record();
    }

    fn note_failure(
        &mut self,
        segment: Option<u32>,
        root: usize,
        op: FailureOp,
        error: &io::Error,
    ) -> FailureDecision {
        let kind = self.push_error(segment, root, op, error);
        self.root_failures[root] = self.root_failures[root].saturating_add(1);
        if !kind.disables_root() && self.root_failures[root] < self.config.attempts_per_root {
            return FailureDecision::Retry { root };
        }
        self.root_disabled[root] = true;
        if let Some(next) = self.first_usable_root() {
            self.active_root = next;
            FailureDecision::Fallback {
                from: root,
                to: next,
            }
        } else {
            self.manifest.state = SessionState::Failed;
            FailureDecision::Exhausted
        }
    }

    fn first_usable_root(&mut self) -> Option<usize> {
        for root in 0..self.config.roots.len() {
            if self.root_disabled[root] {
                continue;
            }
            match self.storage.available_bytes(&self.config.roots[root]) {
                Ok(available) if available < self.config.min_free_bytes => {
                    self.push_error(
                        None,
                        root,
                        FailureOp::Preflight,
                        &io::Error::from(io::ErrorKind::StorageFull),
                    );
                    self.root_disabled[root] = true;
                }
                _ => return Some(root),
            }
        }
        None
    }

    fn push_error(
        &mut self,
        segment: Option<u32>,
        root: usize,
        op: FailureOp,
        error: &io::Error,
    ) -> FailureKind {
        let kind = FailureKind::classify(error);
        if let Some(last) = self.manifest.errors.last_mut()
            && last.segment == segment
            && last.root == root
            && last.op == op
            && last.kind == kind
        {
            last.count = last.count.saturating_add(1);
            return kind;
        }
        if self.manifest.errors.len() == self.config.max_errors {
            self.manifest.errors.remove(0);
            self.manifest.errors_dropped += 1;
        }
        self.manifest.errors.push(SessionErrorRecord {
            sequence: self.next_error_sequence,
            segment,
            root,
            op,
            kind,
            count: 1,
        });
        self.next_error_sequence += 1;
        kind
    }

    /// Writes the next manifest generation to every root, so the newest
    /// state also reaches a root that was disabled for media but can still
    /// take a small file. Succeeds when at least one root took it; failures
    /// are recorded only for enabled roots, where they are unexpected.
    fn persist(&mut self) -> Result<(), io::ErrorKind> {
        self.manifest.generation += 1;
        let text = self.manifest.render();
        let file_name = self.config.manifest_file_name();
        let mut written = false;
        let mut failures = Vec::new();
        for root in 0..self.config.roots.len() {
            match write_manifest_atomic(&self.config.roots[root], &file_name, &text) {
                Ok(()) => written = true,
                Err(error) if !self.root_disabled[root] => failures.push((root, error)),
                Err(_) => {}
            }
        }
        let last_kind = failures.last().map(|(_, error)| error.kind());
        for (root, error) in failures {
            self.push_error(None, root, FailureOp::Manifest, &error);
        }
        self.manifest_stale = !written;
        if written {
            Ok(())
        } else {
            Err(last_kind.unwrap_or(io::ErrorKind::NotFound))
        }
    }

    fn persist_or_record(&mut self) {
        // Failures are already in the error ring and `manifest_stale`.
        let _ = self.persist();
    }
}

fn write_manifest_atomic(root: &Path, file_name: &str, text: &str) -> io::Result<()> {
    let temporary = root.join(format!(".{file_name}.tmp"));
    let result = (|| {
        let mut file = File::create(&temporary)?;
        Write::write_all(&mut file, text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, root.join(file_name))?;
        sync_directory_io(root)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// What recovery found for one root's manifest copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestRead {
    Missing,
    Valid { generation: u64 },
    Corrupt { reason: &'static str },
    Unreadable { kind: io::ErrorKind },
}

/// What recovery did with one segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SegmentOutcome {
    /// Complete or finalized; untouched.
    Intact {
        scan: Fmp4Scan,
    },
    /// A torn tail was truncated to the last complete fragment.
    Repaired {
        scan: Fmp4Scan,
        truncated_bytes: u64,
    },
    /// The init segment is missing or invalid; the file was left untouched.
    Unrecoverable {
        scan: Fmp4Scan,
    },
    /// The manifest named an open segment whose file was never created.
    NeverCreated,
    /// The file is gone, or its root is no longer configured.
    Missing,
    Unreadable {
        kind: io::ErrorKind,
    },
}

/// Recovery result for one segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveredSessionSegment {
    pub index: u32,
    pub root: usize,
    pub path: Option<PathBuf>,
    /// Manifest entry, or `None` for an orphan file adopted from disk.
    pub recorded: Option<SegmentEntry>,
    pub outcome: SegmentOutcome,
}

impl RecoveredSessionSegment {
    fn scan(&self) -> Option<&Fmp4Scan> {
        match &self.outcome {
            SegmentOutcome::Intact { scan }
            | SegmentOutcome::Repaired { scan, .. }
            | SegmentOutcome::Unrecoverable { scan } => Some(scan),
            _ => None,
        }
    }

    /// Playable bytes after recovery.
    #[must_use]
    pub fn kept_bytes(&self) -> u64 {
        self.scan()
            .filter(|scan| scan.is_recoverable())
            .map_or(0, |scan| scan.valid_length)
    }

    /// Bytes that were on disk but are not playable.
    #[must_use]
    pub fn discarded_bytes(&self) -> u64 {
        self.scan().map_or(0, Fmp4Scan::unusable_bytes)
    }

    /// Bytes the manifest recorded as complete that are no longer on disk,
    /// for example after power loss before the page cache was flushed.
    #[must_use]
    pub fn shortfall_bytes(&self) -> u64 {
        let recorded = self.recorded.map_or(0, |entry| entry.bytes);
        recorded.saturating_sub(self.kept_bytes())
    }

    #[must_use]
    pub fn recovered_duration_nanos(&self) -> u64 {
        self.scan()
            .filter(|scan| scan.is_recoverable())
            .map_or(0, Fmp4Scan::recovered_duration_nanos)
    }

    /// Whether some loss in this segment has no exactly known media span.
    #[must_use]
    pub fn has_unknown_loss(&self) -> bool {
        match &self.outcome {
            SegmentOutcome::Intact { .. } | SegmentOutcome::NeverCreated => {
                self.shortfall_bytes() > 0
            }
            SegmentOutcome::Repaired { scan, .. } => {
                scan.lost_fragment.is_none() || self.shortfall_bytes() > 0
            }
            // An empty file the manifest never credited with bytes held
            // nothing: a segment that failed before its init segment landed.
            SegmentOutcome::Unrecoverable { scan } => {
                scan.file_length > 0 || self.shortfall_bytes() > 0
            }
            SegmentOutcome::Missing | SegmentOutcome::Unreadable { .. } => true,
        }
    }
}

/// Full crash-recovery report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecovery {
    pub manifests: Vec<ManifestRead>,
    /// The manifest recovery trusted, if any copy was valid.
    pub manifest: Option<SessionManifest>,
    pub segments: Vec<RecoveredSessionSegment>,
    /// Generation of the rewritten manifest, when one was written.
    pub rewritten_generation: Option<u64>,
    /// Roots that received the rewritten manifest.
    pub rewritten_roots: Vec<usize>,
}

impl SessionRecovery {
    /// Session state recorded before recovery.
    #[must_use]
    pub fn previous_state(&self) -> Option<SessionState> {
        self.manifest.as_ref().map(|manifest| manifest.state)
    }

    #[must_use]
    pub fn recovered_duration_nanos(&self) -> u64 {
        self.segments
            .iter()
            .map(RecoveredSessionSegment::recovered_duration_nanos)
            .sum()
    }

    #[must_use]
    pub fn kept_bytes(&self) -> u64 {
        self.segments
            .iter()
            .map(RecoveredSessionSegment::kept_bytes)
            .sum()
    }

    #[must_use]
    pub fn discarded_bytes(&self) -> u64 {
        self.segments
            .iter()
            .map(RecoveredSessionSegment::discarded_bytes)
            .sum()
    }

    /// Media span of torn fragments whose `moof` survived, in nanoseconds.
    #[must_use]
    pub fn known_lost_duration_nanos(&self) -> u64 {
        self.segments
            .iter()
            .filter_map(|segment| segment.scan().and_then(Fmp4Scan::lost_duration_nanos))
            .sum()
    }

    /// Segments with loss whose media span is not exactly known.
    #[must_use]
    pub fn segments_with_unknown_loss(&self) -> usize {
        self.segments
            .iter()
            .filter(|segment| segment.has_unknown_loss())
            .count()
    }
}

/// Recovers a crashed or interrupted session.
///
/// Reads the manifest copy from every root and trusts the valid one with the
/// highest generation, adopts orphan segment files named for the session,
/// validates every segment, truncates torn tails to the last complete
/// fragment, and rewrites the manifest in state [`SessionState::Recovered`]
/// to every root that exists. Running it again is idempotent apart from the
/// generation counter.
///
/// # Errors
///
/// Returns an error only when a root directory listing fails for a reason
/// other than the directory being absent.
#[allow(clippy::too_many_lines)]
pub fn recover_session(config: &SessionConfig) -> Result<SessionRecovery, SessionError> {
    let manifest_name = config.manifest_file_name();
    let mut manifests = Vec::with_capacity(config.roots.len());
    let mut trusted: Option<SessionManifest> = None;
    for directory in &config.roots {
        let _ = fs::remove_file(directory.join(format!(".{manifest_name}.tmp")));
        let read = read_manifest(&directory.join(&manifest_name), config.name());
        if let Ok(manifest) = &read
            && trusted
                .as_ref()
                .is_none_or(|current| manifest.generation > current.generation)
        {
            trusted = Some(manifest.clone());
        }
        manifests.push(match read {
            Ok(manifest) => ManifestRead::Valid {
                generation: manifest.generation,
            },
            Err(status) => status,
        });
    }

    let mut candidates: Vec<(u32, usize, Option<SegmentEntry>)> = trusted
        .iter()
        .flat_map(|manifest| manifest.segments.iter())
        .map(|entry| (entry.index, entry.root, Some(*entry)))
        .collect();
    for (root, directory) in config.roots.iter().enumerate() {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(SessionError::Io {
                    path: directory.clone(),
                    source,
                });
            }
        };
        for entry in entries.flatten() {
            let Some(index) = entry
                .file_name()
                .to_str()
                .and_then(|name| config.parse_segment_file_name(name))
            else {
                continue;
            };
            if !candidates
                .iter()
                .any(|(known, known_root, _)| *known == index && *known_root == root)
            {
                candidates.push((index, root, None));
            }
        }
    }
    candidates.sort_by_key(|(index, root, _)| (*index, *root));

    let segments: Vec<RecoveredSessionSegment> = candidates
        .into_iter()
        .map(|(index, root, recorded)| {
            let path = config.segment_path(root, index);
            let outcome = path.as_deref().map_or(SegmentOutcome::Missing, |path| {
                recover_segment(path, recorded)
            });
            RecoveredSessionSegment {
                index,
                root,
                path,
                recorded,
                outcome,
            }
        })
        .collect();

    let mut recovery = SessionRecovery {
        manifests,
        manifest: trusted,
        segments,
        rewritten_generation: None,
        rewritten_roots: Vec::new(),
    };
    if recovery.manifest.is_none() && recovery.segments.is_empty() {
        return Ok(recovery);
    }
    let previous = recovery.manifest.as_ref();
    let rewritten = SessionManifest {
        name: config.name.clone(),
        generation: recovery
            .manifests
            .iter()
            .filter_map(|read| match read {
                ManifestRead::Valid { generation } => Some(*generation),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            + 1,
        state: SessionState::Recovered,
        segments: recovery
            .segments
            .iter()
            .filter(|segment| segment.outcome != SegmentOutcome::NeverCreated)
            .map(|segment| {
                let kept = segment.kept_bytes();
                SegmentEntry {
                    index: segment.index,
                    root: segment.root,
                    state: if kept > 0 {
                        SegmentState::Recovered
                    } else {
                        SegmentState::Lost
                    },
                    bytes: kept,
                }
            })
            .collect(),
        errors: previous.map_or_else(Vec::new, |manifest| manifest.errors.clone()),
        errors_dropped: previous.map_or(0, |manifest| manifest.errors_dropped),
    };
    let text = rewritten.render();
    for (root, directory) in config.roots.iter().enumerate() {
        if directory.is_dir() && write_manifest_atomic(directory, &manifest_name, &text).is_ok() {
            recovery.rewritten_roots.push(root);
        }
    }
    if !recovery.rewritten_roots.is_empty() {
        recovery.rewritten_generation = Some(rewritten.generation);
    }
    Ok(recovery)
}

fn read_manifest(path: &Path, name: &str) -> Result<SessionManifest, ManifestRead> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ManifestRead::Missing);
        }
        Err(error) => return Err(ManifestRead::Unreadable { kind: error.kind() }),
    };
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ManifestRead::Unreadable { kind: error.kind() })?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(ManifestRead::Corrupt {
            reason: "manifest exceeds its bound",
        });
    }
    let text = String::from_utf8(bytes).map_err(|_| ManifestRead::Corrupt {
        reason: "manifest is not UTF-8",
    })?;
    let manifest =
        SessionManifest::parse(&text).map_err(|reason| ManifestRead::Corrupt { reason })?;
    if manifest.name != name {
        return Err(ManifestRead::Corrupt {
            reason: "manifest names another session",
        });
    }
    Ok(manifest)
}

fn recover_segment(path: &Path, recorded: Option<SegmentEntry>) -> SegmentOutcome {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return SegmentOutcome::Unreadable {
                kind: io::ErrorKind::InvalidInput,
            };
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let never_created =
                recorded.is_some_and(|entry| entry.state == SegmentState::Open && entry.bytes == 0);
            return if never_created {
                SegmentOutcome::NeverCreated
            } else {
                SegmentOutcome::Missing
            };
        }
        Err(error) => return SegmentOutcome::Unreadable { kind: error.kind() },
    }
    match repair_fmp4_file(path) {
        Ok(repair) => match repair.scan.integrity {
            Fmp4Integrity::Unrecoverable { .. } => {
                SegmentOutcome::Unrecoverable { scan: repair.scan }
            }
            Fmp4Integrity::Torn { .. } => SegmentOutcome::Repaired {
                truncated_bytes: repair.truncated_bytes,
                scan: repair.scan,
            },
            Fmp4Integrity::Complete | Fmp4Integrity::Finalized => {
                SegmentOutcome::Intact { scan: repair.scan }
            }
        },
        Err(error) => SegmentOutcome::Unreadable {
            kind: error.source.kind(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips_and_rejects_tampering() {
        let manifest = SessionManifest {
            name: "show".to_owned(),
            generation: 7,
            state: SessionState::Recording,
            segments: vec![
                SegmentEntry {
                    index: 1,
                    root: 0,
                    state: SegmentState::Closed,
                    bytes: 4096,
                },
                SegmentEntry {
                    index: 2,
                    root: 1,
                    state: SegmentState::Open,
                    bytes: 0,
                },
            ],
            errors: vec![SessionErrorRecord {
                sequence: 1,
                segment: Some(2),
                root: 0,
                op: FailureOp::Write,
                kind: FailureKind::StorageFull,
                count: 3,
            }],
            errors_dropped: 2,
        };
        let text = manifest.render();
        assert_eq!(SessionManifest::parse(&text), Ok(manifest));
        let tampered = text.replace("closed 4096", "closed 4097");
        assert_eq!(SessionManifest::parse(&tampered), Err("checksum mismatch"));
        assert_eq!(
            SessionManifest::parse(&text[..text.len() - 3]),
            Err("missing checksum line")
        );
    }

    #[test]
    fn config_validates_names_roots_and_bounds() {
        assert!(SessionConfig::new("show_1.take-2", "a").is_ok());
        for bad in ["", ".hidden", "a/b", "a b", &"x".repeat(65)] {
            assert!(SessionConfig::new(bad, "a").is_err(), "{bad:?}");
        }
        let config = SessionConfig::new("show", "a").unwrap();
        assert!(config.clone().with_fallback("a").is_err());
        let full = config
            .with_fallback("b")
            .and_then(|config| config.with_fallback("c"))
            .and_then(|config| config.with_fallback("d"))
            .unwrap();
        assert!(full.clone().with_fallback("e").is_err());
        assert!(full.clone().with_max_segments(0).is_err());
        assert!(full.clone().with_attempts_per_root(0).is_err());
        assert_eq!(full.segment_file_name(12), "show-0012.mp4");
        assert_eq!(full.parse_segment_file_name("show-0012.mp4"), Some(12));
        for other in [
            "show-12.mp4",
            "show-0000.mp4",
            "shows-0001.mp4",
            "show-0001.mkv",
        ] {
            assert_eq!(full.parse_segment_file_name(other), None, "{other}");
        }
    }

    #[test]
    fn bare_first_segment_names_segment_one_after_the_session() {
        let config = SessionConfig::new("program-002", "a")
            .unwrap()
            .with_bare_first_segment();
        assert_eq!(config.segment_file_name(1), "program-002.mp4");
        assert_eq!(config.segment_file_name(2), "program-002-0002.mp4");
        assert_eq!(config.parse_segment_file_name("program-002.mp4"), Some(1));
        assert_eq!(
            config.parse_segment_file_name("program-002-0002.mp4"),
            Some(2)
        );
        // Segment 1 has exactly one name, and a sibling session's bare file
        // never parses as one of this session's segments.
        for other in [
            "program-002-0001.mp4",
            "program-002.mkv",
            "program-0020.mp4",
            "program.mp4",
        ] {
            assert_eq!(config.parse_segment_file_name(other), None, "{other}");
        }
        let plain = SessionConfig::new("program", "a").unwrap();
        assert_eq!(plain.parse_segment_file_name("program.mp4"), None);
        assert_eq!(
            plain
                .with_bare_first_segment()
                .parse_segment_file_name("program-002.mp4"),
            None,
            "a three-digit sibling session is not a segment"
        );
    }

    #[test]
    fn classifies_raw_and_kind_errors() {
        assert_eq!(
            FailureKind::classify(&io::Error::from(io::ErrorKind::StorageFull)),
            FailureKind::StorageFull
        );
        #[cfg(unix)]
        assert_eq!(
            FailureKind::classify(&io::Error::from_raw_os_error(28)),
            FailureKind::StorageFull
        );
        assert_eq!(
            FailureKind::classify(&io::Error::other("eio")),
            FailureKind::Other
        );
        assert!(FailureKind::ReadOnly.disables_root());
        assert!(!FailureKind::Other.disables_root());
    }
}
