//! OR-003 fault-tolerant recording: fragmented MP4 crash repair, crash-safe
//! session manifests, disk-full/I/O failover, and free-space policy.
//!
//! Synthetic fragmented MP4 streams are built from raw ISO BMFF boxes so every
//! truncation point can be checked exactly. The `FFmpeg` scenarios at the end
//! decode real H.264/AAC output after failover, truncation, and a `SIGKILL`ed
//! muxer; they skip when `ffmpeg`/`ffprobe` are unavailable unless
//! `FM_REQUIRE_FFMPEG=1`.

use std::{
    cell::Cell,
    env, fs,
    fs::File,
    io::{self, Read, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fm_record::{
    FailureDecision, FailureKind, FailureOp, FileStorage, Fmp4Integrity, Fmp4Splitter, Fmp4Unit,
    ManifestRead, RecordingSession, ResilientFmp4Writer, ResilientWriterConfig, SegmentOutcome,
    SegmentSink, SegmentState, SegmentStorage, SessionConfig, SessionError, SessionManifest,
    SessionState, SpaceProbe, SplitError, UnknownSpace, WriterError, recover_session,
    repair_fmp4_file, scan_fmp4, scan_fmp4_file,
};

const CHILD_ROOT: &str = "FM_RECORD_FMP4_CRASH_CHILD_ROOT";
const SECOND: u64 = 1_000_000_000;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Temp directories
// ---------------------------------------------------------------------------

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_nanos();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "fm-record-or003-{}-{nonce}-{counter}",
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

// ---------------------------------------------------------------------------
// Synthetic fragmented MP4
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct TrackSpec {
    id: u32,
    timescale: u32,
    handler: [u8; 4],
    sample_duration: u32,
    sample_size: u32,
    samples: u32,
}

/// One second of video per fragment at 25 fps in a 90 kHz timescale.
const VIDEO: TrackSpec = TrackSpec {
    id: 1,
    timescale: 90_000,
    handler: *b"vide",
    sample_duration: 3_600,
    sample_size: 40,
    samples: 25,
};

/// One second of audio per fragment: 50 packets of 960 samples at 48 kHz.
const AUDIO: TrackSpec = TrackSpec {
    id: 2,
    timescale: 48_000,
    handler: *b"soun",
    sample_duration: 960,
    sample_size: 6,
    samples: 50,
};

const TRACKS: [TrackSpec; 2] = [VIDEO, AUDIO];

fn mp4_box(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(payload.len() + 8)
        .expect("test box fits")
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(&kind);
    bytes.extend_from_slice(payload);
    bytes
}

fn full_box(kind: [u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = ((u32::from(version) << 24) | flags).to_be_bytes().to_vec();
    body.extend_from_slice(payload);
    mp4_box(kind, &body)
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn init_segment(tracks: &[TrackSpec], fragmented: bool) -> Vec<u8> {
    let ftyp = mp4_box(*b"ftyp", b"isom\0\0\x02\0isomiso6");
    let mvhd = full_box(*b"mvhd", 0, 0, &[0u8; 96]);
    let mut moov = mvhd;
    for track in tracks {
        let mut tkhd = vec![0u8; 8];
        tkhd.extend_from_slice(&track.id.to_be_bytes());
        tkhd.extend_from_slice(&[0u8; 68]);
        let mut media_header = vec![0u8; 8];
        media_header.extend_from_slice(&track.timescale.to_be_bytes());
        media_header.extend_from_slice(&[0u8; 8]);
        let mut hdlr = vec![0u8; 4];
        hdlr.extend_from_slice(&track.handler);
        hdlr.extend_from_slice(&[0u8; 13]);
        let mdia = mp4_box(
            *b"mdia",
            &cat(&[
                &full_box(*b"mdhd", 0, 0, &media_header),
                &full_box(*b"hdlr", 0, 0, &hdlr),
            ]),
        );
        moov.extend(mp4_box(
            *b"trak",
            &cat(&[&full_box(*b"tkhd", 0, 3, &tkhd), &mdia]),
        ));
    }
    if fragmented {
        let mut mvex = Vec::new();
        for track in tracks {
            let mut trex = track.id.to_be_bytes().to_vec();
            trex.extend_from_slice(&1u32.to_be_bytes());
            trex.extend_from_slice(&track.sample_duration.to_be_bytes());
            trex.extend_from_slice(&track.sample_size.to_be_bytes());
            trex.extend_from_slice(&0u32.to_be_bytes());
            mvex.extend(full_box(*b"trex", 0, 0, &trex));
        }
        moov.extend(mp4_box(*b"mvex", &mvex));
    }
    cat(&[&ftyp, &mp4_box(*b"moov", &moov)])
}

fn traf(track: &TrackSpec, decode_time: u64, data_offset: i32, explicit_base: bool) -> Vec<u8> {
    let tfhd = if explicit_base {
        let mut body = track.id.to_be_bytes().to_vec();
        body.extend_from_slice(&0u64.to_be_bytes());
        full_box(*b"tfhd", 0, 0x1, &body)
    } else {
        full_box(*b"tfhd", 0, 0x2_0000, &track.id.to_be_bytes())
    };
    let tfdt = full_box(*b"tfdt", 1, 0, &decode_time.to_be_bytes());
    let mut trun = track.samples.to_be_bytes().to_vec();
    trun.extend_from_slice(&data_offset.to_be_bytes());
    for _ in 0..track.samples {
        trun.extend_from_slice(&track.sample_duration.to_be_bytes());
        trun.extend_from_slice(&track.sample_size.to_be_bytes());
    }
    mp4_box(
        *b"traf",
        &cat(&[&tfhd, &tfdt, &full_box(*b"trun", 0, 0x301, &trun)]),
    )
}

fn data_len(track: &TrackSpec) -> usize {
    (track.samples * track.sample_size) as usize
}

/// Fragment `number` (1-based) covering media second `number - 1`.
fn fragment_with(
    number: u32,
    tracks: &[TrackSpec],
    explicit_base: bool,
    extra_data: i32,
) -> Vec<u8> {
    let build = |offsets: &[i32]| {
        let mut moof = full_box(*b"mfhd", 0, 0, &number.to_be_bytes());
        for (track, offset) in tracks.iter().zip(offsets) {
            let decode = u64::from(number - 1) * u64::from(track.samples * track.sample_duration);
            moof.extend(traf(track, decode, *offset, explicit_base));
        }
        mp4_box(*b"moof", &moof)
    };
    let moof_len = build(&vec![0; tracks.len()]).len();
    let mut offsets = Vec::new();
    let mut cursor = moof_len + 8;
    for track in tracks {
        offsets.push(i32::try_from(cursor).unwrap() + extra_data);
        cursor += data_len(track);
    }
    let moof = build(&offsets);
    let mut data = Vec::new();
    for track in tracks {
        data.extend(std::iter::repeat_n(
            number.to_le_bytes()[0],
            data_len(track),
        ));
    }
    cat(&[&moof, &mp4_box(*b"mdat", &data)])
}

fn fragment(number: u32) -> Vec<u8> {
    fragment_with(number, &TRACKS, false, 0)
}

fn mfra() -> Vec<u8> {
    mp4_box(*b"mfra", &full_box(*b"mfro", 0, 0, &16u32.to_be_bytes()))
}

/// Init segment plus `count` fragments, and the cumulative fragment end
/// offsets (the first entry is the init length).
fn stream(count: u32) -> (Vec<u8>, Vec<u64>) {
    let mut bytes = init_segment(&TRACKS, true);
    let mut boundaries = vec![bytes.len() as u64];
    for number in 1..=count {
        bytes.extend(fragment(number));
        boundaries.push(bytes.len() as u64);
    }
    (bytes, boundaries)
}

fn scan_bytes(bytes: &[u8]) -> fm_record::Fmp4Scan {
    scan_fmp4(&mut io::Cursor::new(bytes), bytes.len() as u64).expect("in-memory scan")
}

// ---------------------------------------------------------------------------
// Fragmented MP4 scanning and repair
// ---------------------------------------------------------------------------

#[test]
fn scan_reports_complete_and_finalized_files_with_exact_spans() {
    let (bytes, boundaries) = stream(3);
    let scan = scan_bytes(&bytes);
    assert_eq!(scan.integrity, Fmp4Integrity::Complete);
    assert_eq!(scan.init_length, boundaries[0]);
    assert_eq!(scan.valid_length, bytes.len() as u64);
    assert_eq!(scan.fragments, 3);
    assert_eq!(
        (scan.first_sequence, scan.last_sequence),
        (Some(1), Some(3))
    );
    assert_eq!(scan.sequence_gaps, 0);
    let movie = scan.movie.as_ref().expect("movie");
    assert_eq!(movie.tracks().len(), 2);
    assert_eq!(movie.tracks()[0].handler, *b"vide");
    assert_eq!(scan.spans.len(), 2);
    assert_eq!(scan.spans[0].samples, 75);
    assert_eq!(scan.spans[1].samples, 150);
    assert_eq!(scan.spans[0].duration_nanos(), 3 * SECOND);
    assert_eq!(scan.recovered_duration_nanos(), 3 * SECOND);

    let mut finalized = bytes.clone();
    finalized.extend(mfra());
    let scan = scan_bytes(&finalized);
    assert_eq!(scan.integrity, Fmp4Integrity::Finalized);
    assert_eq!(scan.valid_length, finalized.len() as u64);

    let mut skipped = bytes;
    skipped.extend(fragment(5));
    assert_eq!(scan_bytes(&skipped).sequence_gaps, 1);
}

#[test]
fn every_truncation_point_keeps_exactly_the_complete_fragments() {
    let (bytes, boundaries) = stream(3);
    let fragment_len = boundaries[3] - boundaries[2];
    for cut in 0..=bytes.len() as u64 {
        let scan = scan_bytes(&bytes[..usize::try_from(cut).unwrap()]);
        if cut < boundaries[0] {
            assert!(
                matches!(scan.integrity, Fmp4Integrity::Unrecoverable { .. }),
                "cut {cut}: {:?}",
                scan.integrity
            );
            assert_eq!(scan.valid_length, 0);
            continue;
        }
        let kept = boundaries.iter().rposition(|end| *end <= cut).unwrap();
        assert_eq!(scan.valid_length, boundaries[kept], "cut {cut}");
        assert_eq!(scan.fragments, kept as u64, "cut {cut}");
        assert_eq!(scan.recovered_duration_nanos(), kept as u64 * SECOND);
        assert_eq!(scan.unusable_bytes(), cut - boundaries[kept]);
        if cut == boundaries[kept] {
            assert_eq!(scan.integrity, Fmp4Integrity::Complete, "cut {cut}");
        } else {
            assert!(
                matches!(scan.integrity, Fmp4Integrity::Torn { offset, .. } if offset == boundaries[kept]),
                "cut {cut}: {:?}",
                scan.integrity
            );
            // Once the moof and the mdat header survive, the lost span is
            // known exactly: one second of media.
            let mdat_payload_start =
                boundaries[kept] + fragment_len - (data_len(&VIDEO) + data_len(&AUDIO)) as u64;
            if cut >= mdat_payload_start {
                assert_eq!(scan.lost_duration_nanos(), Some(SECOND), "cut {cut}");
            }
        }
    }
}

#[test]
fn repair_truncates_a_torn_file_on_disk_and_is_idempotent() {
    let temp = TempDirectory::new();
    let (bytes, boundaries) = stream(4);
    let path = temp.join("torn.mp4");
    let cut = boundaries[3] + (boundaries[4] - boundaries[3]) / 2;
    fs::write(&path, &bytes[..usize::try_from(cut).unwrap()]).unwrap();

    let repair = repair_fmp4_file(&path).expect("repair");
    assert_eq!(repair.truncated_bytes, cut - boundaries[3]);
    assert_eq!(fs::metadata(&path).unwrap().len(), boundaries[3]);
    assert_eq!(
        fs::read(&path).unwrap(),
        &bytes[..usize::try_from(boundaries[3]).unwrap()]
    );

    let again = repair_fmp4_file(&path).expect("second repair");
    assert_eq!(again.truncated_bytes, 0);
    assert_eq!(again.scan.integrity, Fmp4Integrity::Complete);
    assert_eq!(again.scan.fragments, 3);
}

#[test]
fn zero_filled_tail_from_power_loss_is_truncated() {
    let temp = TempDirectory::new();
    let (mut bytes, boundaries) = stream(2);
    bytes.extend(std::iter::repeat_n(0u8, 4096));
    let path = temp.join("zeros.mp4");
    fs::write(&path, &bytes).unwrap();
    let repair = repair_fmp4_file(&path).unwrap();
    assert!(matches!(
        repair.scan.integrity,
        Fmp4Integrity::Torn {
            reason: "invalid box type",
            ..
        }
    ));
    assert_eq!(repair.truncated_bytes, 4096);
    assert_eq!(scan_fmp4_file(&path).unwrap().valid_length, boundaries[2]);
}

#[test]
fn an_inconsistent_fragment_ends_the_playable_prefix() {
    let (mut bytes, boundaries) = stream(1);
    // The run claims data starting one byte past where it really is, so its
    // last byte would fall outside the mdat.
    bytes.extend(fragment_with(2, &TRACKS, false, 1));
    bytes.extend(fragment(3));
    let scan = scan_bytes(&bytes);
    assert_eq!(scan.fragments, 1);
    assert_eq!(
        scan.integrity,
        Fmp4Integrity::Torn {
            reason: "track run data lies outside its mdat",
            offset: boundaries[1]
        }
    );

    let (mut bytes, _) = stream(1);
    bytes.extend(mp4_box(*b"mdat", b"orphan"));
    assert!(matches!(
        scan_bytes(&bytes).integrity,
        Fmp4Integrity::Torn {
            reason: "mdat without moof",
            ..
        }
    ));

    let (mut bytes, boundaries) = stream(1);
    let mut unknown_track = VIDEO;
    unknown_track.id = 9;
    bytes.extend(fragment_with(2, &[unknown_track], false, 0));
    assert_eq!(
        scan_bytes(&bytes).integrity,
        Fmp4Integrity::Torn {
            reason: "traf names an unknown track",
            offset: boundaries[1]
        }
    );
}

#[test]
fn files_without_a_fragmented_init_segment_are_unrecoverable_and_untouched() {
    let temp = TempDirectory::new();
    let cases: [(&str, Vec<u8>, &str); 3] = [
        (
            "progressive.mp4",
            cat(&[&init_segment(&TRACKS, false), &fragment(1)]),
            "moov lacks mvex; not a fragmented MP4",
        ),
        ("no-ftyp.mp4", fragment(1), "file does not start with ftyp"),
        (
            "media-first.mp4",
            cat(&[&mp4_box(*b"ftyp", b"isom\0\0\0\0"), &fragment(1)]),
            "media precedes a fragmented moov",
        ),
    ];
    for (name, bytes, reason) in cases {
        let path = temp.join(name);
        fs::write(&path, &bytes).unwrap();
        let repair = repair_fmp4_file(&path).unwrap();
        assert!(
            matches!(repair.scan.integrity, Fmp4Integrity::Unrecoverable { reason: found, .. } if found == reason),
            "{name}: {:?}",
            repair.scan.integrity
        );
        assert_eq!(repair.truncated_bytes, 0);
        assert_eq!(fs::read(&path).unwrap(), bytes, "{name} was modified");
    }
}

// ---------------------------------------------------------------------------
// Stream splitting
// ---------------------------------------------------------------------------

#[test]
fn splitter_cuts_any_chunking_into_relocatable_units() {
    let (mut bytes, boundaries) = stream(4);
    bytes.extend(mfra());
    let mut state = 0x2545_f491_u32;
    for _ in 0..20 {
        let mut splitter = Fmp4Splitter::new(1 << 20);
        let mut units = Vec::new();
        let mut position = 0;
        while position < bytes.len() {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let step = 1 + (state >> 16) as usize % 700;
            let end = (position + step).min(bytes.len());
            splitter.push(&bytes[position..end], &mut units).unwrap();
            position = end;
        }
        splitter.finish(&mut units).unwrap();
        assert_eq!(units.len(), 6);
        let rebuilt: Vec<u8> = units
            .iter()
            .flat_map(|unit| match unit {
                Fmp4Unit::Init { bytes, .. }
                | Fmp4Unit::Fragment { bytes, .. }
                | Fmp4Unit::Index { bytes } => bytes.clone(),
            })
            .collect();
        assert_eq!(rebuilt, bytes);
        assert!(
            matches!(&units[0], Fmp4Unit::Init { bytes, .. } if bytes.len() as u64 == boundaries[0])
        );
        for (number, unit) in units[1..5].iter().enumerate() {
            let Fmp4Unit::Fragment { fragment, bytes } = unit else {
                panic!("expected fragment");
            };
            assert_eq!(fragment.sequence_number, u32::try_from(number).unwrap() + 1);
            assert_eq!(fragment.offset, boundaries[number]);
            assert_eq!(
                bytes.len() as u64,
                boundaries[number + 1] - boundaries[number]
            );
            assert_eq!(fragment.runs[0].base_decode_time, number as u64 * 90_000);
        }
        assert!(matches!(units[5], Fmp4Unit::Index { .. }));
    }
}

#[test]
fn splitter_refuses_unrelocatable_oversized_and_truncated_streams() {
    let mut units = Vec::new();
    let absolute = cat(&[
        &init_segment(&TRACKS, true),
        &fragment_with(1, &TRACKS, true, 0),
    ]);
    let mut splitter = Fmp4Splitter::new(1 << 20);
    assert!(matches!(
        splitter.push(&absolute, &mut units),
        Err(SplitError::Invalid {
            reason: "explicit base data offset is not relocatable",
            ..
        })
    ));
    assert_eq!(splitter.push(b"", &mut units), Err(SplitError::Poisoned));

    let mut huge = init_segment(&TRACKS, true);
    huge.extend_from_slice(&(10u32 << 20).to_be_bytes());
    huge.extend_from_slice(b"moof");
    let mut splitter = Fmp4Splitter::new(1 << 20);
    units.clear();
    assert!(matches!(
        splitter.push(&huge, &mut units),
        Err(SplitError::UnitTooLarge { .. })
    ));

    let (bytes, boundaries) = stream(2);
    let mut splitter = Fmp4Splitter::new(1 << 20);
    units.clear();
    splitter
        .push(
            &bytes[..usize::try_from(boundaries[2]).unwrap() - 5],
            &mut units,
        )
        .unwrap();
    assert_eq!(units.len(), 2);
    assert!(matches!(
        splitter.finish(&mut units),
        Err(SplitError::Truncated { .. })
    ));
}

// ---------------------------------------------------------------------------
// Fault-injecting storage
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct RootFaults {
    /// Bytes this root accepts before `StorageFull` (partial writes happen).
    capacity: Option<u64>,
    /// Transient `Other` write failures to inject before succeeding.
    transient_writes: u32,
    /// Fail every segment creation with this kind.
    create_error: Option<io::ErrorKind>,
    /// Reported free space; `None` reports unknown.
    available: Option<u64>,
}

struct FaultyStorage {
    roots: Vec<(PathBuf, Rc<RootState>)>,
}

struct RootState {
    faults: std::cell::RefCell<RootFaults>,
    used: Cell<u64>,
}

impl FaultyStorage {
    fn new(roots: &[(PathBuf, RootFaults)]) -> Self {
        Self {
            roots: roots
                .iter()
                .map(|(path, faults)| {
                    (
                        path.clone(),
                        Rc::new(RootState {
                            faults: std::cell::RefCell::new(faults.clone()),
                            used: Cell::new(0),
                        }),
                    )
                })
                .collect(),
        }
    }

    fn state(&self, directory: &Path) -> Option<&Rc<RootState>> {
        self.roots
            .iter()
            .find(|(root, _)| root == directory)
            .map(|(_, state)| state)
    }
}

impl SpaceProbe for FaultyStorage {
    fn available_bytes(&mut self, directory: &Path) -> io::Result<u64> {
        self.state(directory)
            .and_then(|state| state.faults.borrow().available)
            .ok_or_else(|| io::Error::from(io::ErrorKind::Unsupported))
    }
}

struct FaultySink {
    file: File,
    length: u64,
    root: Rc<RootState>,
}

impl SegmentSink for FaultySink {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        {
            let mut faults = self.root.faults.borrow_mut();
            if faults.transient_writes > 0 {
                faults.transient_writes -= 1;
                return Err(io::Error::other("injected transient failure"));
            }
        }
        let capacity = self.root.faults.borrow().capacity;
        let allowed = capacity.map_or(u64::MAX, |capacity| {
            capacity.saturating_sub(self.root.used.get())
        });
        let take = usize::try_from(allowed.min(bytes.len() as u64)).unwrap();
        Write::write_all(&mut self.file, &bytes[..take])?;
        self.length += take as u64;
        self.root.used.set(self.root.used.get() + take as u64);
        if take < bytes.len() {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        Ok(())
    }

    fn sync_data(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }

    fn truncate(&mut self, length: u64) -> io::Result<()> {
        use std::io::Seek as _;
        self.file.set_len(length)?;
        self.file.seek(io::SeekFrom::Start(length))?;
        let freed = self.length.saturating_sub(length);
        self.root.used.set(self.root.used.get() - freed);
        self.length = length;
        Ok(())
    }
}

impl SegmentStorage for FaultyStorage {
    type Sink = FaultySink;

    fn create_segment(&mut self, path: &Path) -> io::Result<FaultySink> {
        let root = Rc::clone(
            self.state(path.parent().expect("segment parent"))
                .expect("segment under a configured root"),
        );
        if let Some(kind) = root.faults.borrow().create_error {
            return Err(io::Error::from(kind));
        }
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        Ok(FaultySink {
            file,
            length: 0,
            root,
        })
    }
}

fn read_manifest(path: &Path) -> SessionManifest {
    SessionManifest::parse(&fs::read_to_string(path).expect("read manifest"))
        .expect("parse manifest")
}

fn writer_config(segment_fragments: Option<u64>) -> ResilientWriterConfig {
    ResilientWriterConfig {
        max_unit_bytes: 1 << 20,
        sync_every_fragments: NonZeroU64::new(1),
        max_segment_fragments: segment_fragments.and_then(NonZeroU64::new),
        max_segment_bytes: None,
    }
}

// ---------------------------------------------------------------------------
// Sessions, failover, and recovery
// ---------------------------------------------------------------------------

#[test]
fn rotating_session_writes_playable_segments_and_a_closed_manifest() {
    let temp = TempDirectory::new();
    let config = SessionConfig::new("show", temp.join("primary")).unwrap();
    let session = RecordingSession::start(config.clone(), FileStorage::new(UnknownSpace)).unwrap();
    let mut writer = ResilientFmp4Writer::new(session, writer_config(Some(2)));
    let (mut bytes, _) = stream(5);
    bytes.extend(mfra());
    for chunk in bytes.chunks(333) {
        writer.push(chunk).unwrap();
    }
    let stats = writer.stats();
    let snapshot = writer.finish().unwrap();
    assert_eq!(snapshot.state, SessionState::Stopped);
    assert_eq!(snapshot.segments, 3);
    assert_eq!(snapshot.errors, []);
    assert_eq!(stats.fragments_written, 5);
    assert_eq!(stats.segments_opened, 3);

    let manifest = read_manifest(&temp.join("primary/show.fmsession"));
    assert_eq!(manifest.state, SessionState::Stopped);
    let init_len = init_segment(&TRACKS, true).len() as u64;
    let fragment_len = fragment(1).len() as u64;
    let expected_fragments = [2, 2, 1];
    for (entry, fragments) in manifest.segments.iter().zip(expected_fragments) {
        assert_eq!(entry.state, SegmentState::Closed);
        assert_eq!(entry.bytes, init_len + fragments * fragment_len);
        let scan = scan_fmp4_file(&config.segment_path(0, entry.index).unwrap()).unwrap();
        assert_eq!(
            scan.integrity,
            Fmp4Integrity::Complete,
            "mfra must be dropped"
        );
        assert_eq!(scan.fragments, fragments);
    }

    let recovery = recover_session(&config).unwrap();
    assert_eq!(recovery.previous_state(), Some(SessionState::Stopped));
    assert_eq!(recovery.recovered_duration_nanos(), 5 * SECOND);
    assert_eq!(recovery.discarded_bytes(), 0);
    assert_eq!(recovery.segments_with_unknown_loss(), 0);
    assert!(
        recovery
            .segments
            .iter()
            .all(|segment| matches!(segment.outcome, SegmentOutcome::Intact { .. }))
    );
}

#[test]
fn finish_report_stops_the_session_when_the_stream_ends_inside_a_fragment() {
    let temp = TempDirectory::new();
    let config = SessionConfig::new("killed", temp.join("primary"))
        .unwrap()
        .with_bare_first_segment();
    let session = RecordingSession::start(config.clone(), FileStorage::new(UnknownSpace)).unwrap();
    let mut writer = ResilientFmp4Writer::new(session, writer_config(None));
    let (bytes, boundaries) = stream(3);
    // A killed muxer: the stream stops 10 bytes into the fourth unit's tail.
    let cut = usize::try_from(boundaries[2]).unwrap() + 10;
    writer.push(&bytes[..cut]).unwrap();
    let finish = writer.finish_report();
    assert!(
        matches!(
            finish.error,
            Some(WriterError::Stream(SplitError::Truncated { buffered: 10 }))
        ),
        "{:?}",
        finish.error
    );
    assert_eq!(finish.discarded_bytes, 10);
    assert_eq!(finish.stats.fragments_written, 2);
    let snapshot = finish.snapshot.unwrap();
    assert_eq!(snapshot.state, SessionState::Stopped);
    assert_eq!(finish.segments.len(), 1);
    assert_eq!(finish.segments[0].state, SegmentState::Closed);
    assert_eq!(finish.segments[0].bytes, boundaries[2]);

    // Segment 1 carries the bare session name and needs no repair.
    let path = temp.join("primary/killed.mp4");
    assert_eq!(config.segment_path(0, 1).unwrap(), path);
    assert_eq!(
        scan_fmp4_file(&path).unwrap().integrity,
        Fmp4Integrity::Complete
    );
    let manifest = read_manifest(&temp.join("primary/killed.fmsession"));
    assert_eq!(manifest.state, SessionState::Stopped);
}

#[test]
fn bare_first_segment_recovery_repairs_the_operator_named_file() {
    let temp = TempDirectory::new();
    let (primary, fallback) = (temp.join("primary"), temp.join("fallback"));
    let config = SessionConfig::new("program", &primary)
        .unwrap()
        .with_fallback(&fallback)
        .unwrap()
        .with_bare_first_segment();
    let mut session =
        RecordingSession::start(config.clone(), FileStorage::new(UnknownSpace)).unwrap();
    let opened = session.open_segment().unwrap();
    assert_eq!(opened.path, primary.join("program.mp4"));
    let (bytes, boundaries) = stream(3);
    let mut sink = opened.sink;
    // Crash: a torn third fragment and no close.
    Write::write_all(
        &mut sink,
        &bytes[..usize::try_from(boundaries[3]).unwrap() - 7],
    )
    .unwrap();
    drop(sink);
    drop(session);

    let recovery = recover_session(&config).unwrap();
    assert_eq!(recovery.previous_state(), Some(SessionState::Recording));
    assert_eq!(recovery.segments.len(), 1);
    assert!(
        matches!(
            recovery.segments[0].outcome,
            SegmentOutcome::Repaired { .. }
        ),
        "{:?}",
        recovery.segments[0].outcome
    );
    assert_eq!(recovery.kept_bytes(), boundaries[2]);
    assert_eq!(
        fs::metadata(primary.join("program.mp4")).unwrap().len(),
        boundaries[2]
    );
    assert_eq!(recovery.rewritten_roots, [0, 1]);
}

#[test]
fn disk_full_mid_fragment_fails_over_without_losing_media() {
    let temp = TempDirectory::new();
    let (primary, fallback) = (temp.join("primary"), temp.join("fallback"));
    let init_len = init_segment(&TRACKS, true).len() as u64;
    let fragment_len = fragment(1).len() as u64;
    // The primary fills up half-way through the third fragment.
    let capacity = init_len + 2 * fragment_len + fragment_len / 2;
    let storage = FaultyStorage::new(&[
        (
            primary.clone(),
            RootFaults {
                capacity: Some(capacity),
                ..RootFaults::default()
            },
        ),
        (fallback.clone(), RootFaults::default()),
    ]);
    let config = SessionConfig::new("show", &primary)
        .unwrap()
        .with_fallback(&fallback)
        .unwrap();
    let session = RecordingSession::start(config.clone(), storage).unwrap();
    let mut writer = ResilientFmp4Writer::new(session, writer_config(None));
    let (bytes, _) = stream(6);
    writer.push(&bytes).unwrap();
    let stats = writer.stats();
    assert_eq!(stats.fragments_written, 6);
    assert_eq!(stats.fragments_rewritten, 1);
    assert_eq!(stats.failovers, 1);
    let snapshot = writer.session().snapshot();
    assert_eq!(snapshot.active_root, 1);
    assert_eq!(snapshot.disabled_roots, [0]);
    assert_eq!(snapshot.errors.len(), 1);
    assert_eq!(
        (
            snapshot.errors[0].op,
            snapshot.errors[0].kind,
            snapshot.errors[0].root
        ),
        (FailureOp::Write, FailureKind::StorageFull, 0)
    );
    let snapshot = writer.finish().unwrap();
    assert_eq!(snapshot.state, SessionState::Stopped);

    // Primary: truncated back to its two complete fragments.
    let first = scan_fmp4_file(&config.segment_path(0, 1).unwrap()).unwrap();
    assert_eq!(first.integrity, Fmp4Integrity::Complete);
    assert_eq!(first.fragments, 2);
    // Fallback: init plus the rewritten third fragment onward.
    let second = scan_fmp4_file(&config.segment_path(1, 2).unwrap()).unwrap();
    assert_eq!(second.integrity, Fmp4Integrity::Complete);
    assert_eq!((second.first_sequence, second.fragments), (Some(3), 4));
    assert_eq!(second.spans[0].first_decode_time, 2 * 90_000);

    // A root disabled for media still receives the small manifest when it
    // can, so both copies carry the newest generation.
    let primary_manifest = read_manifest(&primary.join("show.fmsession"));
    let fallback_manifest = read_manifest(&fallback.join("show.fmsession"));
    assert_eq!(fallback_manifest, primary_manifest);
    assert_eq!(fallback_manifest.segments[0].state, SegmentState::Failed);
    assert_eq!(
        fallback_manifest.segments[0].bytes,
        init_len + 2 * fragment_len
    );
    assert_eq!(fallback_manifest.segments[1].state, SegmentState::Closed);

    let recovery = recover_session(&config).unwrap();
    assert_eq!(recovery.recovered_duration_nanos(), 6 * SECOND);
    assert_eq!(recovery.segments_with_unknown_loss(), 0);
    assert_eq!(recovery.discarded_bytes(), 0);
    assert_eq!(
        recovery.manifest.as_ref().unwrap().generation,
        fallback_manifest.generation
    );
}

#[test]
fn transient_failures_retry_the_same_root_before_falling_back() {
    let temp = TempDirectory::new();
    let (primary, fallback) = (temp.join("primary"), temp.join("fallback"));
    let storage = FaultyStorage::new(&[
        (
            primary.clone(),
            RootFaults {
                transient_writes: 1,
                ..RootFaults::default()
            },
        ),
        (fallback.clone(), RootFaults::default()),
    ]);
    let config = SessionConfig::new("show", &primary)
        .unwrap()
        .with_fallback(&fallback)
        .unwrap()
        .with_attempts_per_root(2)
        .unwrap();
    let session = RecordingSession::start(config.clone(), storage).unwrap();
    let mut writer = ResilientFmp4Writer::new(session, writer_config(None));
    let (bytes, _) = stream(2);
    writer.push(&bytes).unwrap();
    assert_eq!(writer.stats().retries, 1);
    assert_eq!(writer.stats().failovers, 0);
    assert_eq!(writer.session().snapshot().active_root, 0);

    // Two more consecutive transient failures exhaust the primary's budget.
    writer
        .session_mut()
        .storage_mut()
        .state(&primary)
        .unwrap()
        .faults
        .borrow_mut()
        .transient_writes = 2;
    writer.push(&fragment(3)).unwrap();
    assert_eq!(writer.stats().failovers, 1);
    assert_eq!(writer.session().snapshot().active_root, 1);
    writer.finish().unwrap();
    let recovery = recover_session(&config).unwrap();
    assert_eq!(recovery.recovered_duration_nanos(), 3 * SECOND);
    assert_eq!(recovery.segments_with_unknown_loss(), 0);
}

#[test]
fn exhausting_every_root_fails_loudly_and_persists_the_failure() {
    let temp = TempDirectory::new();
    let (primary, fallback) = (temp.join("primary"), temp.join("fallback"));
    let init_len = init_segment(&TRACKS, true).len() as u64;
    let fragment_len = fragment(1).len() as u64;
    let storage = FaultyStorage::new(&[
        (
            primary.clone(),
            RootFaults {
                capacity: Some(init_len + fragment_len + 10),
                ..RootFaults::default()
            },
        ),
        (
            fallback.clone(),
            RootFaults {
                create_error: Some(io::ErrorKind::ReadOnlyFilesystem),
                ..RootFaults::default()
            },
        ),
    ]);
    let config = SessionConfig::new("show", &primary)
        .unwrap()
        .with_fallback(&fallback)
        .unwrap();
    let session = RecordingSession::start(config.clone(), storage).unwrap();
    let mut writer = ResilientFmp4Writer::new(session, writer_config(None));
    let (bytes, _) = stream(3);
    let error = writer.push(&bytes).unwrap_err();
    let WriterError::Session(SessionError::Exhausted { last: Some(last) }) = error else {
        panic!("expected exhaustion, got {error:?}");
    };
    assert_eq!(
        (last.op, last.kind, last.root),
        (FailureOp::CreateSegment, FailureKind::ReadOnly, 1)
    );
    let snapshot = writer.session().snapshot();
    assert_eq!(snapshot.state, SessionState::Failed);
    assert_eq!(snapshot.disabled_roots, [0, 1]);
    assert!(matches!(
        writer.push(&fragment(4)),
        Err(WriterError::Session(SessionError::Exhausted { .. }))
    ));
    let manifest = read_manifest(&primary.join("show.fmsession"));
    assert_eq!(manifest.state, SessionState::Failed);
    assert_eq!(manifest.errors.len(), 2);
    assert_eq!(manifest.errors[0].kind, FailureKind::StorageFull);

    // What reached disk before the failure is still recoverable.
    let recovery = recover_session(&config).unwrap();
    assert_eq!(recovery.previous_state(), Some(SessionState::Failed));
    assert_eq!(recovery.recovered_duration_nanos(), SECOND);
}

#[test]
fn error_ring_is_bounded_and_folds_repeats() {
    let temp = TempDirectory::new();
    let roots: Vec<PathBuf> = (0..4)
        .map(|root| temp.join(&format!("root{root}")))
        .collect();
    let mut config = SessionConfig::new("show", &roots[0])
        .unwrap()
        .with_max_errors(2)
        .unwrap()
        .with_attempts_per_root(3)
        .unwrap();
    for root in &roots[1..] {
        config = config.with_fallback(root).unwrap();
    }
    let faults: Vec<(PathBuf, RootFaults)> = roots
        .iter()
        .map(|root| {
            (
                root.clone(),
                RootFaults {
                    create_error: Some(io::ErrorKind::Other),
                    ..RootFaults::default()
                },
            )
        })
        .collect();
    let mut session = RecordingSession::start(config, FaultyStorage::new(&faults)).unwrap();
    let Err(error) = session.open_segment() else {
        panic!("every root fails segment creation");
    };
    assert!(matches!(error, SessionError::Exhausted { .. }));
    let snapshot = session.snapshot();
    assert_eq!(snapshot.errors.len(), 2);
    // Four roots, each failing three times with distinct segment indexes.
    assert_eq!(snapshot.errors_dropped, 10);
    assert_eq!(snapshot.errors[1].root, 3);
    assert_eq!(snapshot.state, SessionState::Failed);

    let mut storage_session = RecordingSession::start(
        SessionConfig::new("folded", temp.join("folded")).unwrap(),
        FileStorage::new(UnknownSpace),
    )
    .unwrap();
    let _opened = storage_session.open_segment().unwrap();
    for _ in 0..5 {
        storage_session.record_incident(FailureOp::Truncate, &io::Error::other("eio"));
    }
    let errors = storage_session.snapshot().errors;
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].count, 5);
}

#[test]
fn low_free_space_skips_a_root_at_start_and_existing_sessions_are_refused() {
    let temp = TempDirectory::new();
    let (primary, fallback) = (temp.join("primary"), temp.join("fallback"));
    let storage = FaultyStorage::new(&[
        (
            primary.clone(),
            RootFaults {
                available: Some(1_000),
                ..RootFaults::default()
            },
        ),
        (
            fallback.clone(),
            RootFaults {
                available: Some(1 << 30),
                ..RootFaults::default()
            },
        ),
    ]);
    let config = SessionConfig::new("show", &primary)
        .unwrap()
        .with_fallback(&fallback)
        .unwrap()
        .with_min_free_bytes(1 << 20);
    let mut session = RecordingSession::start(config.clone(), storage).unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.active_root, 1);
    assert_eq!(snapshot.errors[0].op, FailureOp::Preflight);
    let opened = session.open_segment().unwrap();
    assert_eq!((opened.root, opened.index), (1, 1));
    assert!(opened.path.starts_with(&fallback));

    assert!(matches!(
        RecordingSession::start(config, FileStorage::new(UnknownSpace)),
        Err(SessionError::ManifestExists { root: 0 })
    ));

    let all_low = FaultyStorage::new(&[(
        temp.join("low"),
        RootFaults {
            available: Some(10),
            ..RootFaults::default()
        },
    )]);
    let config = SessionConfig::new("low", temp.join("low"))
        .unwrap()
        .with_min_free_bytes(100);
    assert!(matches!(
        RecordingSession::start(config, all_low),
        Err(SessionError::NoUsableRoot)
    ));
}

#[test]
fn failure_policy_decisions_follow_the_error_class() {
    let temp = TempDirectory::new();
    let (primary, fallback) = (temp.join("primary"), temp.join("fallback"));
    let config = SessionConfig::new("show", &primary)
        .unwrap()
        .with_fallback(&fallback)
        .unwrap()
        .with_attempts_per_root(2)
        .unwrap();
    let mut session = RecordingSession::start(config, FileStorage::new(UnknownSpace)).unwrap();
    let _first = session.open_segment().unwrap();
    assert_eq!(
        session
            .record_failure(
                FailureOp::Write,
                &io::Error::from(io::ErrorKind::FileTooLarge),
                10
            )
            .unwrap(),
        FailureDecision::Retry { root: 0 }
    );
    let _second = session.open_segment().unwrap();
    session.mark_healthy();
    assert_eq!(
        session
            .record_failure(FailureOp::Sync, &io::Error::other("eio"), 10)
            .unwrap(),
        FailureDecision::Retry { root: 0 }
    );
    let _third = session.open_segment().unwrap();
    assert_eq!(
        session
            .record_failure(FailureOp::Write, &io::Error::other("eio"), 10)
            .unwrap(),
        FailureDecision::Fallback { from: 0, to: 1 }
    );
    let _fourth = session.open_segment().unwrap();
    assert_eq!(
        session
            .record_failure(
                FailureOp::Write,
                &io::Error::from(io::ErrorKind::PermissionDenied),
                0
            )
            .unwrap(),
        FailureDecision::Exhausted
    );
    assert!(matches!(
        session.record_failure(FailureOp::Write, &io::Error::other("x"), 0),
        Err(SessionError::InvalidState(_))
    ));
}

#[test]
fn recovery_adopts_orphans_tolerates_corrupt_manifests_and_reports_never_created() {
    let temp = TempDirectory::new();
    let (primary, fallback) = (temp.join("primary"), temp.join("fallback"));
    let config = SessionConfig::new("show", &primary)
        .unwrap()
        .with_fallback(&fallback)
        .unwrap();
    let mut session =
        RecordingSession::start(config.clone(), FileStorage::new(UnknownSpace)).unwrap();
    let mut opened = session.open_segment().unwrap();
    let (first, _) = stream(2);
    SegmentSink::write_all(&mut opened.sink, &first).unwrap();
    session.close_segment(first.len() as u64).unwrap();
    // A crash right after the manifest names segment 2 but before its file
    // exists: simulate by deleting the created file and dropping the session.
    let second = session.open_segment().unwrap();
    drop(second.sink);
    fs::remove_file(&second.path).unwrap();
    drop(session);
    // An orphan the manifest never heard of.
    let (orphan, _) = stream(1);
    fs::write(config.segment_path(1, 7).unwrap(), &orphan).unwrap();
    // A corrupt primary manifest copy and a leftover temporary file.
    let manifest_path = primary.join("show.fmsession");
    let mut text = fs::read(&manifest_path).unwrap();
    text[40] ^= 0x01;
    fs::write(&manifest_path, &text).unwrap();
    fs::write(primary.join(".show.fmsession.tmp"), b"partial").unwrap();

    let recovery = recover_session(&config).unwrap();
    assert!(matches!(
        recovery.manifests[0],
        ManifestRead::Corrupt { .. }
    ));
    assert!(matches!(recovery.manifests[1], ManifestRead::Valid { .. }));
    assert!(!primary.join(".show.fmsession.tmp").exists());
    assert_eq!(recovery.previous_state(), Some(SessionState::Recording));
    let outcomes: Vec<(u32, usize, bool, &SegmentOutcome)> = recovery
        .segments
        .iter()
        .map(|segment| {
            (
                segment.index,
                segment.root,
                segment.recorded.is_some(),
                &segment.outcome,
            )
        })
        .collect();
    assert_eq!(outcomes.len(), 3);
    assert!(matches!(
        outcomes[0],
        (1, 0, true, SegmentOutcome::Intact { .. })
    ));
    assert!(matches!(
        outcomes[1],
        (2, 0, true, SegmentOutcome::NeverCreated)
    ));
    assert!(matches!(
        outcomes[2],
        (7, 1, false, SegmentOutcome::Intact { .. })
    ));
    assert_eq!(recovery.recovered_duration_nanos(), 3 * SECOND);
    assert_eq!(recovery.segments_with_unknown_loss(), 0);
    assert_eq!(recovery.rewritten_roots, [0, 1]);

    let rewritten = read_manifest(&manifest_path);
    assert_eq!(rewritten.state, SessionState::Recovered);
    assert_eq!(
        rewritten
            .segments
            .iter()
            .map(|entry| (entry.index, entry.state))
            .collect::<Vec<_>>(),
        [(1, SegmentState::Recovered), (7, SegmentState::Recovered)]
    );
}

#[test]
fn missing_and_shortened_segments_are_reported_as_unknown_loss() {
    let temp = TempDirectory::new();
    let config = SessionConfig::new("show", temp.join("primary")).unwrap();
    let session = RecordingSession::start(config.clone(), FileStorage::new(UnknownSpace)).unwrap();
    let mut writer = ResilientFmp4Writer::new(session, writer_config(Some(2)));
    let (bytes, boundaries) = stream(4);
    writer.push(&bytes).unwrap();
    writer.finish().unwrap();
    // Power loss lost the page cache for the end of segment 1 and all of
    // segment 2, even though the manifest recorded them closed.
    let first = config.segment_path(0, 1).unwrap();
    let shortened = boundaries[1] + 10;
    File::options()
        .write(true)
        .open(&first)
        .unwrap()
        .set_len(shortened)
        .unwrap();
    fs::remove_file(config.segment_path(0, 2).unwrap()).unwrap();

    let recovery = recover_session(&config).unwrap();
    let first = &recovery.segments[0];
    assert!(matches!(
        first.outcome,
        SegmentOutcome::Repaired {
            truncated_bytes: 10,
            ..
        }
    ));
    assert_eq!(first.kept_bytes(), boundaries[1]);
    assert_eq!(first.shortfall_bytes(), boundaries[2] - boundaries[1]);
    assert!(first.has_unknown_loss());
    assert_eq!(recovery.segments[1].outcome, SegmentOutcome::Missing);
    assert_eq!(recovery.segments_with_unknown_loss(), 2);
    assert_eq!(recovery.recovered_duration_nanos(), SECOND);
}

// ---------------------------------------------------------------------------
// Process kill during a fragment write
// ---------------------------------------------------------------------------

struct TearingStorage {
    tear_at: u64,
    written: u64,
    marker: PathBuf,
}

impl SpaceProbe for TearingStorage {
    fn available_bytes(&mut self, _directory: &Path) -> io::Result<u64> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

struct TearingSink {
    file: File,
    tear_at: u64,
    written: u64,
    marker: PathBuf,
}

impl SegmentSink for TearingSink {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let end = self.written + bytes.len() as u64;
        if end <= self.tear_at {
            Write::write_all(&mut self.file, bytes)?;
            self.written = end;
            return Ok(());
        }
        let keep = usize::try_from(self.tear_at - self.written).unwrap();
        Write::write_all(&mut self.file, &bytes[..keep])?;
        self.file.sync_all()?;
        fs::write(&self.marker, b"torn").unwrap();
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }

    fn sync_data(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }

    fn truncate(&mut self, length: u64) -> io::Result<()> {
        self.file.set_len(length)
    }
}

impl SegmentStorage for TearingStorage {
    type Sink = TearingSink;

    fn create_segment(&mut self, path: &Path) -> io::Result<TearingSink> {
        let sink = TearingSink {
            file: fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?,
            tear_at: self.tear_at.saturating_sub(self.written),
            written: 0,
            marker: self.marker.clone(),
        };
        Ok(sink)
    }
}

#[test]
fn subprocess_child_tears_a_fragment_write() {
    let Some(root) = env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        return;
    };
    let tear_at: u64 = env::var("FM_RECORD_FMP4_TEAR_AT").unwrap().parse().unwrap();
    let config = SessionConfig::new("show", root.join("recordings")).unwrap();
    let storage = TearingStorage {
        tear_at,
        written: 0,
        marker: root.join("torn-ready"),
    };
    let session = RecordingSession::start(config, storage).unwrap();
    let mut writer = ResilientFmp4Writer::new(session, writer_config(None));
    let (bytes, _) = stream(5);
    writer
        .push(&bytes)
        .expect("parent kills the child during this push");
    panic!("torn write unexpectedly completed");
}

struct ChildGuard(Option<Child>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn killed_writer_recovers_every_complete_fragment_and_reports_the_torn_one() {
    let temp = TempDirectory::new();
    let (_, boundaries) = stream(5);
    // Kill inside the fourth fragment's mdat payload: its moof survives, so
    // the lost span is known exactly.
    let tear_at = boundaries[4] - 30;
    let marker = temp.join("torn-ready");
    let child = Command::new(env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "subprocess_child_tears_a_fragment_write",
            "--nocapture",
        ])
        .env(CHILD_ROOT, temp.path())
        .env("FM_RECORD_FMP4_TEAR_AT", tear_at.to_string())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn child");
    let mut guard = ChildGuard(Some(child));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !marker.exists() {
        let child = guard.0.as_mut().unwrap();
        if let Some(status) = child.try_wait().unwrap() {
            panic!("child exited before tearing: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for torn write"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let mut child = guard.0.take().unwrap();
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());

    let config = SessionConfig::new("show", temp.join("recordings")).unwrap();
    let before = read_manifest(&temp.join("recordings/show.fmsession"));
    assert_eq!(before.state, SessionState::Recording);
    assert_eq!(before.segments[0].state, SegmentState::Open);

    let recovery = recover_session(&config).unwrap();
    assert_eq!(recovery.segments.len(), 1);
    let segment = &recovery.segments[0];
    let SegmentOutcome::Repaired {
        scan,
        truncated_bytes,
    } = &segment.outcome
    else {
        panic!("expected a repaired segment, got {:?}", segment.outcome);
    };
    assert_eq!(*truncated_bytes, tear_at - boundaries[3]);
    assert_eq!(scan.fragments, 3);
    assert_eq!(scan.lost_fragment.as_ref().unwrap().sequence_number, 4);
    assert_eq!(recovery.recovered_duration_nanos(), 3 * SECOND);
    assert_eq!(recovery.known_lost_duration_nanos(), SECOND);
    assert_eq!(recovery.segments_with_unknown_loss(), 0);
    assert_eq!(
        fs::metadata(config.segment_path(0, 1).unwrap())
            .unwrap()
            .len(),
        boundaries[3]
    );
    let after = read_manifest(&temp.join("recordings/show.fmsession"));
    assert_eq!(after.state, SessionState::Recovered);
    assert_eq!(after.segments[0].bytes, boundaries[3]);

    let again = recover_session(&config).unwrap();
    assert_eq!(again.previous_state(), Some(SessionState::Recovered));
    assert!(matches!(
        again.segments[0].outcome,
        SegmentOutcome::Intact { .. }
    ));
}

// ---------------------------------------------------------------------------
// FFmpeg-produced fragmented MP4
// ---------------------------------------------------------------------------

/// Same fragmentation flags as the `fm-codec-ffmpeg` program recorder.
const MOVFLAGS: &str = "+empty_moov+default_base_moof+frag_keyframe";

fn tools_available() -> bool {
    let available = ["ffmpeg", "ffprobe"].iter().all(|tool| {
        Command::new(tool)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    assert!(
        available || env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG=1 but ffmpeg or ffprobe is unavailable"
    );
    if !available {
        eprintln!("skipping: ffmpeg/ffprobe unavailable");
    }
    available
}

fn ffmpeg_args(seconds: u32, realtime: bool) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin"]
        .map(str::to_owned)
        .to_vec();
    if realtime {
        args.push("-re".to_owned());
    }
    args.extend(
        [
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            &seconds.to_string(),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "25",
            "-keyint_min",
            "25",
            "-sc_threshold",
            "0",
            "-c:a",
            "aac",
            "-movflags",
            MOVFLAGS,
            "-flush_packets",
            "1",
            "-f",
            "mp4",
        ]
        .map(str::to_owned),
    );
    args
}

fn ffmpeg_fragmented_mp4(seconds: u32) -> Vec<u8> {
    let mut args = ffmpeg_args(seconds, false);
    args.push("pipe:1".to_owned());
    let output = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Decodes every video frame and returns the count, failing on any decode
/// error.
fn decoded_video_frames(path: &Path) -> u64 {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_read_frames",
            "-of",
            "default=nokey=1:noprint_wrappers=1",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("run ffprobe");
    assert!(
        output.status.success() && output.stderr.is_empty(),
        "ffprobe failed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .expect("frame count")
}

fn video_samples(scan: &fm_record::Fmp4Scan) -> u64 {
    let movie = scan.movie.as_ref().unwrap();
    let video = movie
        .tracks()
        .iter()
        .find(|track| track.handler == *b"vide")
        .unwrap();
    scan.spans
        .iter()
        .find(|span| span.track_id == video.track_id)
        .map_or(0, |span| span.samples)
}

#[test]
fn ffmpeg_output_survives_disk_full_failover_with_every_frame_decodable() {
    if !tools_available() {
        return;
    }
    let temp = TempDirectory::new();
    let bytes = ffmpeg_fragmented_mp4(4);
    let source = temp.join("source.mp4");
    fs::write(&source, &bytes).unwrap();
    let source_scan = scan_fmp4_file(&source).unwrap();
    assert_eq!(source_scan.integrity, Fmp4Integrity::Finalized);
    assert!(source_scan.fragments >= 3);
    let total_frames = decoded_video_frames(&source);
    assert_eq!(total_frames, 100);
    assert_eq!(video_samples(&source_scan), total_frames);

    let (primary, fallback) = (temp.join("primary"), temp.join("fallback"));
    let storage = FaultyStorage::new(&[
        (
            primary.clone(),
            RootFaults {
                capacity: Some(bytes.len() as u64 * 2 / 5),
                ..RootFaults::default()
            },
        ),
        (fallback.clone(), RootFaults::default()),
    ]);
    let config = SessionConfig::new("program", &primary)
        .unwrap()
        .with_fallback(&fallback)
        .unwrap();
    let session = RecordingSession::start(config.clone(), storage).unwrap();
    let mut writer = ResilientFmp4Writer::new(session, writer_config(None));
    for chunk in bytes.chunks(4096) {
        writer.push(chunk).unwrap();
    }
    assert_eq!(writer.stats().failovers, 1);
    writer.finish().unwrap();

    let first = config.segment_path(0, 1).unwrap();
    let second = config.segment_path(1, 2).unwrap();
    let frames = decoded_video_frames(&first) + decoded_video_frames(&second);
    assert_eq!(frames, total_frames, "failover lost or duplicated frames");
    let recovery = recover_session(&config).unwrap();
    assert_eq!(recovery.segments_with_unknown_loss(), 0);
    assert_eq!(recovery.discarded_bytes(), 0);
}

#[test]
fn ffmpeg_output_cut_mid_fragment_repairs_to_a_decodable_file() {
    if !tools_available() {
        return;
    }
    let temp = TempDirectory::new();
    let bytes = ffmpeg_fragmented_mp4(4);
    let full = scan_bytes(&bytes);
    // Shrink from the end (dropping the mfra first) until the last fragment
    // is gone; the valid prefix then ends where that fragment starts.
    let mut cut = bytes.len();
    while scan_bytes(&bytes[..cut]).fragments == full.fragments {
        cut -= 256;
    }
    let last_start = scan_bytes(&bytes[..cut]).valid_length;
    let start = usize::try_from(last_start).unwrap();
    let tear = start + (bytes.len() - start) / 2;
    let path = temp.join("torn.mp4");
    fs::write(&path, &bytes[..tear]).unwrap();

    let repair = repair_fmp4_file(&path).unwrap();
    assert!(matches!(repair.scan.integrity, Fmp4Integrity::Torn { .. }));
    assert_eq!(repair.scan.fragments, full.fragments - 1);
    assert_eq!(fs::metadata(&path).unwrap().len(), last_start);
    let frames = decoded_video_frames(&path);
    assert_eq!(frames, video_samples(&repair.scan));
    assert_eq!(frames, 75);
}

#[test]
fn ffmpeg_killed_mid_recording_leaves_a_repairable_file_with_bounded_loss() {
    if !tools_available() {
        return;
    }
    let temp = TempDirectory::new();
    let path = temp.join("killed.mp4");
    let mut args = ffmpeg_args(30, true);
    args.push("-y".to_owned());
    args.push(path.to_str().unwrap().to_owned());
    let child = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ffmpeg");
    let mut guard = ChildGuard(Some(child));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let fragments = fs::read(&path)
            .ok()
            .map_or(0, |bytes| scan_bytes(&bytes).fragments);
        if fragments >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "ffmpeg produced no fragments");
        thread::sleep(Duration::from_millis(50));
    }
    // Land the kill at an arbitrary point inside the next fragment.
    thread::sleep(Duration::from_millis(400));
    let mut child = guard.0.take().unwrap();
    child.kill().unwrap();
    let _ = child.wait();

    let mut raw = Vec::new();
    File::open(&path).unwrap().read_to_end(&mut raw).unwrap();
    let repair = repair_fmp4_file(&path).unwrap();
    assert!(repair.scan.is_recoverable());
    assert!(repair.scan.fragments >= 2);
    assert_ne!(repair.scan.integrity, Fmp4Integrity::Finalized);
    // Bounded loss: at most the bytes of one partially written fragment were
    // dropped (the 1 s GOP bounds one fragment's size at this resolution).
    assert!(
        repair.truncated_bytes < 256 * 1024,
        "{}",
        repair.truncated_bytes
    );
    let frames = decoded_video_frames(&path);
    assert_eq!(frames, video_samples(&repair.scan));
    assert_eq!(frames, repair.scan.fragments * 25);
}
