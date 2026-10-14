//! Streamed WAV sidecar failover: a WAV byte stream with placeholder sizes
//! (as a muxer writes to a non-seekable destination) is split into whole
//! frames, synced about once per second, and re-homed into a new
//! `<name>-NNNN.wav` segment when a root fills or a sync fails. Every segment
//! must keep a valid, size-correct header and no accepted audio may be lost
//! while any root remains usable.

use std::{
    cell::RefCell,
    collections::HashMap,
    env, fs,
    io::{self, Cursor},
    num::NonZeroU64,
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use fm_record::{
    FileStorage, RecordingSession, ResilientWavConfig, ResilientWavWriter, SegmentFormat,
    SegmentSink, SegmentState, SegmentStorage, SessionConfig, SessionError, SessionState,
    SpaceProbe, UnknownSpace, WavError, WavSegmentOutcome, WavWriterError, read_wav_info,
    recover_session, recover_wav, recover_wav_session,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 48 kHz stereo 16-bit: four bytes per frame, 192 000 bytes per second.
const RATE: u32 = 48_000;
const BLOCK: usize = 4;
const SECOND_BYTES: usize = RATE as usize * BLOCK;

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_nanos();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "fm-record-wav-{}-{nonce}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The header `FFmpeg` 6 writes for `-c:a pcm_s16le -f wav` on a non-seekable
/// output: placeholder RIFF and data sizes and a `LIST`/`INFO` chunk.
fn streaming_header() -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&u32::MAX.to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16_u32.to_le_bytes());
    header.extend_from_slice(&1_u16.to_le_bytes());
    header.extend_from_slice(&2_u16.to_le_bytes());
    header.extend_from_slice(&RATE.to_le_bytes());
    header.extend_from_slice(&(RATE * 4).to_le_bytes());
    header.extend_from_slice(&4_u16.to_le_bytes());
    header.extend_from_slice(&16_u16.to_le_bytes());
    header.extend_from_slice(b"LIST");
    header.extend_from_slice(&26_u32.to_le_bytes());
    header.extend_from_slice(b"INFOISFT");
    header.extend_from_slice(&14_u32.to_le_bytes());
    header.extend_from_slice(b"Lavf60.16.100\0");
    header.extend_from_slice(b"data");
    header.extend_from_slice(&u32::MAX.to_le_bytes());
    header
}

/// `frames` sample frames, each carrying its own index so order and loss are
/// both visible.
fn audio(frames: u32) -> Vec<u8> {
    (0..frames).flat_map(u32::to_le_bytes).collect()
}

fn frames_for_millis(millis: u32) -> u32 {
    RATE / 1_000 * millis
}

/// Feeds `stream` in odd-sized chunks that split headers and frames.
fn push_all<S: SegmentStorage>(
    writer: &mut ResilientWavWriter<S>,
    stream: &[u8],
) -> Result<(), WavWriterError> {
    for chunk in stream.chunks(4_099) {
        writer.push(chunk)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// In-memory storage with per-root capacity and injectable sync failures
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Disk {
    files: HashMap<PathBuf, Vec<u8>>,
    /// Bytes each root can hold; absent roots are unlimited.
    capacity: HashMap<PathBuf, usize>,
    /// Sync calls to fail per root, as transient I/O errors.
    failing_syncs: HashMap<PathBuf, u32>,
}

impl Disk {
    fn used(&self, root: &Path) -> usize {
        self.files
            .iter()
            .filter(|(path, _)| path.parent() == Some(root))
            .map(|(_, bytes)| bytes.len())
            .sum()
    }
}

#[derive(Clone, Default)]
struct MemoryStorage(Rc<RefCell<Disk>>);

impl MemoryStorage {
    fn with_capacity(self, root: &Path, bytes: usize) -> Self {
        self.0.borrow_mut().capacity.insert(root.to_owned(), bytes);
        self
    }

    fn file(&self, path: &Path) -> Vec<u8> {
        self.0.borrow().files[path].clone()
    }
}

impl SpaceProbe for MemoryStorage {
    fn available_bytes(&mut self, _directory: &Path) -> io::Result<u64> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

impl SegmentStorage for MemoryStorage {
    type Sink = MemorySink;

    fn create_segment(&mut self, path: &Path) -> io::Result<MemorySink> {
        let mut disk = self.0.borrow_mut();
        if disk.files.contains_key(path) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        disk.files.insert(path.to_owned(), Vec::new());
        Ok(MemorySink {
            disk: Rc::clone(&self.0),
            path: path.to_owned(),
        })
    }
}

struct MemorySink {
    disk: Rc<RefCell<Disk>>,
    path: PathBuf,
}

impl MemorySink {
    fn root(&self) -> PathBuf {
        self.path.parent().expect("segment has a root").to_owned()
    }
}

impl SegmentSink for MemorySink {
    /// Writes what fits, then fails like a full filesystem.
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let root = self.root();
        let mut disk = self.disk.borrow_mut();
        let available = disk.capacity.get(&root).map_or(usize::MAX, |capacity| {
            capacity.saturating_sub(disk.used(&root))
        });
        let fits = bytes.len().min(available);
        disk.files
            .get_mut(&self.path)
            .expect("segment exists")
            .extend_from_slice(&bytes[..fits]);
        if fits < bytes.len() {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        Ok(())
    }

    fn sync_data(&mut self) -> io::Result<()> {
        let root = self.root();
        let mut disk = self.disk.borrow_mut();
        if let Some(remaining) = disk.failing_syncs.get_mut(&root)
            && *remaining > 0
        {
            *remaining -= 1;
            return Err(io::Error::other("injected sync failure"));
        }
        Ok(())
    }

    fn truncate(&mut self, length: u64) -> io::Result<()> {
        let mut disk = self.disk.borrow_mut();
        let file = disk.files.get_mut(&self.path).expect("segment exists");
        file.truncate(usize::try_from(length).expect("test length fits"));
        Ok(())
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let mut disk = self.disk.borrow_mut();
        let file = disk.files.get_mut(&self.path).expect("segment exists");
        let start = usize::try_from(offset).expect("test offset fits");
        assert!(start + bytes.len() <= file.len(), "patches stay in place");
        file[start..start + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
}

fn wav_config(name: &str, roots: &[&Path]) -> SessionConfig {
    let mut config = SessionConfig::new(name, roots[0])
        .unwrap()
        .with_segment_format(SegmentFormat::Wav)
        .with_bare_first_segment();
    for root in &roots[1..] {
        config = config.with_fallback(*root).unwrap();
    }
    config
}

/// Checks that `file` is a complete, size-correct WAV with the stream's
/// format and returns its sample data.
fn valid_segment_data(file: &[u8]) -> Vec<u8> {
    let header = streaming_header();
    let info = read_wav_info(&mut Cursor::new(file)).expect("segment header parses");
    assert_eq!(info.data_offset, header.len() as u64);
    assert_eq!(info.spec.channels, 2);
    assert_eq!(info.spec.sample_rate, RATE);
    assert!(!info.rf64);
    assert_eq!(info.riff_size, file.len() as u64 - 8, "RIFF size is exact");
    assert_eq!(
        info.data_bytes,
        file.len() as u64 - info.data_offset,
        "data size is exact"
    );
    assert_eq!(info.data_bytes % BLOCK as u64, 0, "whole frames only");
    // The format and metadata chunks are the stream's own, byte for byte.
    assert_eq!(file[8..header.len() - 4], header[8..header.len() - 4]);
    let mut copy = Cursor::new(file.to_vec());
    assert!(
        recover_wav(&mut copy).unwrap().was_consistent,
        "nothing left for recovery to repair"
    );
    file[header.len()..].to_vec()
}

/// Every manifest segment, in index order, as (path, file bytes).
fn segment_files(
    config: &SessionConfig,
    storage: &MemoryStorage,
    segments: &[fm_record::SegmentEntry],
) -> Vec<(PathBuf, Vec<u8>)> {
    let mut entries = segments.to_vec();
    entries.sort_by_key(|entry| entry.index);
    entries
        .iter()
        .map(|entry| {
            let path = config.segment_path(entry.root, entry.index).unwrap();
            let bytes = storage.file(&path);
            (path, bytes)
        })
        .collect()
}

#[test]
fn a_root_that_fills_mid_recording_fails_over_without_losing_audio() {
    let primary = TempDirectory::new();
    let fallback = TempDirectory::new();
    let header = streaming_header();
    // Room for the header plus 3.5 seconds of audio on the primary.
    let storage =
        MemoryStorage::default().with_capacity(primary.path(), header.len() + SECOND_BYTES * 7 / 2);
    let config = wav_config("show", &[primary.path(), fallback.path()]);
    let session = RecordingSession::start(config.clone(), storage.clone()).unwrap();
    let mut writer = ResilientWavWriter::new(session, ResilientWavConfig::default());

    let samples = audio(frames_for_millis(6_250));
    let mut stream = header.clone();
    stream.extend_from_slice(&samples);
    // A trailing partial frame never reaches a segment.
    stream.extend_from_slice(&[0xAB, 0xCD]);
    push_all(&mut writer, &stream).unwrap();
    assert!(writer.held_bytes() < SECOND_BYTES + 4_099);
    let finish = writer.finish_report();
    assert!(finish.error.is_none(), "{:?}", finish.error);
    assert_eq!(finish.discarded_bytes, 2);
    let snapshot = finish.snapshot.unwrap();
    assert_eq!(snapshot.state, SessionState::Stopped);
    assert_eq!(snapshot.disabled_roots, vec![0]);
    assert_eq!(finish.stats.failovers, 1);
    assert_eq!(finish.stats.segments_opened, 2);
    assert_eq!(finish.stats.data_bytes, samples.len() as u64);
    assert_eq!(finish.stats.frames_written, samples.len() as u64 / 4);
    // The failed segment keeps exactly its synced seconds; the half second
    // after the last sync is rewritten into the fallback segment.
    let rewritten = finish.stats.bytes_rewritten;
    assert!(
        rewritten >= SECOND_BYTES as u64 / 2 && rewritten < SECOND_BYTES as u64 / 2 + 4_100,
        "{rewritten}"
    );

    let files = segment_files(&config, &storage, &finish.segments);
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].0, primary.join("show.wav"));
    assert_eq!(files[1].0, fallback.join("show-0002.wav"));
    assert_eq!(finish.segments[0].state, SegmentState::Failed);
    assert_eq!(finish.segments[1].state, SegmentState::Closed);
    for (entry, (_, bytes)) in finish.segments.iter().zip(&files) {
        assert_eq!(entry.bytes, bytes.len() as u64, "manifest names every byte");
    }
    let mut recorded = Vec::new();
    for (index, (_, bytes)) in files.iter().enumerate() {
        let data = valid_segment_data(bytes);
        if index == 0 {
            assert_eq!(data.len(), SECOND_BYTES * 3, "trimmed to the last sync");
        }
        recorded.extend(data);
    }
    assert_eq!(recorded.len(), samples.len());
    assert!(recorded == samples, "every frame survives in order");
}

#[test]
fn a_failed_sync_retries_on_the_same_root_and_rewrites_held_audio() {
    let primary = TempDirectory::new();
    let storage = MemoryStorage::default();
    storage
        .0
        .borrow_mut()
        .failing_syncs
        .insert(primary.path().to_owned(), 1);
    let config = wav_config("retry", &[primary.path()]);
    let session = RecordingSession::start(config.clone(), storage.clone()).unwrap();
    let mut writer = ResilientWavWriter::new(session, ResilientWavConfig::default());
    let samples = audio(frames_for_millis(2_500));
    let mut stream = streaming_header();
    stream.extend_from_slice(&samples);
    push_all(&mut writer, &stream).unwrap();
    let finish = writer.finish_report();
    assert!(finish.error.is_none(), "{:?}", finish.error);
    assert_eq!(finish.stats.retries, 1);
    assert_eq!(finish.stats.failovers, 0);
    assert_eq!(finish.stats.bytes_rewritten, SECOND_BYTES as u64);
    let files = segment_files(&config, &storage, &finish.segments);
    assert_eq!(files.len(), 2);
    // The first segment failed its first sync, so it keeps only a header.
    assert_eq!(valid_segment_data(&files[0].1), Vec::<u8>::new());
    let mut recorded = Vec::new();
    for (path, bytes) in &files {
        assert_eq!(path.parent(), Some(primary.path()));
        recorded.extend(valid_segment_data(bytes));
    }
    assert_eq!(recorded, samples);
}

#[test]
fn segments_rotate_at_the_configured_size_and_stay_independently_valid() {
    let primary = TempDirectory::new();
    let storage = MemoryStorage::default();
    let config = wav_config("rotate", &[primary.path()]);
    let session = RecordingSession::start(config.clone(), storage.clone()).unwrap();
    let mut writer = ResilientWavWriter::new(
        session,
        ResilientWavConfig {
            // Not a whole number of frames: rounded down.
            max_segment_data_bytes: NonZeroU64::new(SECOND_BYTES as u64 + 3),
            ..ResilientWavConfig::default()
        },
    );
    let samples = audio(frames_for_millis(3_500));
    let mut stream = streaming_header();
    stream.extend_from_slice(&samples);
    push_all(&mut writer, &stream).unwrap();
    let finish = writer.finish_report();
    assert!(finish.error.is_none(), "{:?}", finish.error);
    let files = segment_files(&config, &storage, &finish.segments);
    let lengths: Vec<usize> = files
        .iter()
        .map(|(_, bytes)| valid_segment_data(bytes).len())
        .collect();
    assert_eq!(
        lengths,
        vec![SECOND_BYTES, SECOND_BYTES, SECOND_BYTES, SECOND_BYTES / 2]
    );
    let recorded: Vec<u8> = files
        .iter()
        .flat_map(|(_, bytes)| valid_segment_data(bytes))
        .collect();
    assert_eq!(recorded, samples);
}

#[test]
fn every_root_full_reports_exhaustion_and_keeps_valid_synced_audio() {
    let primary = TempDirectory::new();
    let fallback = TempDirectory::new();
    let header = streaming_header();
    let storage = MemoryStorage::default()
        .with_capacity(primary.path(), header.len() + SECOND_BYTES * 3 / 2)
        .with_capacity(fallback.path(), header.len() + SECOND_BYTES * 3 / 2);
    let config = wav_config("full", &[primary.path(), fallback.path()]);
    let session = RecordingSession::start(config.clone(), storage.clone()).unwrap();
    let mut writer = ResilientWavWriter::new(session, ResilientWavConfig::default());
    let samples = audio(frames_for_millis(5_000));
    let mut stream = header;
    stream.extend_from_slice(&samples);
    let error = push_all(&mut writer, &stream).unwrap_err();
    assert!(
        matches!(
            error,
            WavWriterError::Session(SessionError::Exhausted { .. })
        ),
        "{error:?}"
    );
    let finish = writer.finish_report();
    let snapshot = finish.snapshot.unwrap();
    assert_eq!(snapshot.state, SessionState::Failed);
    // Each root kept its last synced second, and the fallback starts where
    // the primary's durable audio ended.
    let files = segment_files(&config, &storage, &finish.segments);
    let recorded: Vec<u8> = files
        .iter()
        .filter(|(_, bytes)| !bytes.is_empty())
        .flat_map(|(_, bytes)| valid_segment_data(bytes))
        .collect();
    assert_eq!(recorded.len(), SECOND_BYTES * 2);
    assert_eq!(recorded, samples[..SECOND_BYTES * 2]);
}

#[test]
fn a_header_only_stream_leaves_one_valid_empty_segment() {
    let primary = TempDirectory::new();
    let storage = MemoryStorage::default();
    let config = wav_config("empty", &[primary.path()]);
    let session = RecordingSession::start(config.clone(), storage.clone()).unwrap();
    let mut writer = ResilientWavWriter::new(session, ResilientWavConfig::default());
    writer.push(&streaming_header()).unwrap();
    let finish = writer.finish_report();
    assert!(finish.error.is_none());
    let files = segment_files(&config, &storage, &finish.segments);
    assert_eq!(files.len(), 1);
    assert_eq!(valid_segment_data(&files[0].1), Vec::<u8>::new());

    // A stream that ends inside its header writes nothing and says so.
    let other = TempDirectory::new();
    let session =
        RecordingSession::start(wav_config("cut", &[other.path()]), MemoryStorage::default())
            .unwrap();
    let mut writer = ResilientWavWriter::new(session, ResilientWavConfig::default());
    writer.push(&streaming_header()[..20]).unwrap();
    let finish = writer.finish_report();
    assert_eq!(finish.discarded_bytes, 20);
    assert!(matches!(
        finish.error,
        Some(WavWriterError::Stream(WavError::Truncated))
    ));
    assert_eq!(finish.segments.len(), 0);
}

#[test]
fn wav_sessions_name_their_files_beside_the_mp4_session() {
    let directory = TempDirectory::new();
    let config = wav_config("program-002", &[directory.path()]);
    assert_eq!(config.segment_file_name(1), "program-002.wav");
    assert_eq!(config.segment_file_name(2), "program-002-0002.wav");
    assert_eq!(config.manifest_file_name(), "program-002.wav.fmsession");
    // The MP4 session of the same name starts in the same directory.
    let mp4 = SessionConfig::new("program-002", directory.path())
        .unwrap()
        .with_bare_first_segment();
    let _mp4 = RecordingSession::start(mp4, FileStorage::new(UnknownSpace)).unwrap();
    let _wav = RecordingSession::start(config.clone(), FileStorage::new(UnknownSpace)).unwrap();
    assert!(matches!(
        recover_session(&config),
        Err(SessionError::InvalidConfig(_))
    ));
}

#[test]
fn a_crashed_writer_is_repaired_by_wav_session_recovery() {
    let primary = TempDirectory::new();
    let config = wav_config("crash", &[primary.path()]);
    let session = RecordingSession::start(config.clone(), FileStorage::new(UnknownSpace)).unwrap();
    let mut writer = ResilientWavWriter::new(session, ResilientWavConfig::default());
    let samples = audio(frames_for_millis(2_500));
    let mut stream = streaming_header();
    stream.extend_from_slice(&samples);
    push_all(&mut writer, &stream).unwrap();
    // A crash: the writer never finishes, so the header keeps its
    // placeholder sizes, and a torn write left half a frame behind.
    drop(writer);
    let path = primary.join("crash.wav");
    let mut torn = fs::read(&path).unwrap();
    torn.extend_from_slice(&[1, 2]);
    fs::write(&path, &torn).unwrap();

    let recovery = recover_wav_session(&config).unwrap();
    assert_eq!(recovery.previous_state(), Some(SessionState::Recording));
    assert_eq!(recovery.segments.len(), 1);
    let segment = &recovery.segments[0];
    let WavSegmentOutcome::Repaired { recovery: repair } = &segment.outcome else {
        panic!("unexpected outcome {:?}", segment.outcome);
    };
    assert_eq!(repair.discarded_tail_bytes, 2);
    assert!(!segment.has_unknown_loss());
    assert_eq!(
        segment.recovered_frames(),
        u64::from(frames_for_millis(2_500))
    );
    assert_eq!(recovery.recovered_duration_nanos(), 2_500_000_000);
    assert!(recovery.rewritten_generation.is_some());
    let repaired = fs::read(&path).unwrap();
    assert_eq!(valid_segment_data(&repaired), samples);

    // Running it again finds nothing to repair.
    let again = recover_wav_session(&config).unwrap();
    assert!(matches!(
        again.segments[0].outcome,
        WavSegmentOutcome::Intact { .. }
    ));
}
