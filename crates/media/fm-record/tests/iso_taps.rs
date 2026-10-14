use std::{
    ffi::OsString,
    fs,
    num::{NonZeroU32, NonZeroU64, NonZeroU128},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use fm_frame::{
    ClockDomainId, CodecConfigGeneration, CodecId, EncodedPacket, EncodedPacketMetadata,
    MediaTiming, NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, PacketFlags,
    SequenceNumber, StreamId,
};
use fm_record::{
    ActionOutcome, ActionReceiptId, EnqueueError, ISO_MANIFEST_NAME, IsoManifestError,
    IsoTapConfig, IsoTapError, IsoTapSession, IsoTapSnapshot, MAX_ISO_TAPS, QueueLimits,
    RecordEvent, RecorderCoordinator, RecorderError, RecorderId, RecorderState, SegmentPolicy,
    TimecodeSource, parse_iso_manifest, render_iso_manifest_entries,
};
use fm_types::{InputId, MediaTimestamp, PixelFormat, TimeBase, Timecode, VideoDimensions};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_nanos();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-record-iso-{name}-{}-{nonce}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(value: u128) -> RecorderId {
    RecorderId::new(NonZeroU128::new(value).expect("nonzero recorder id"))
}

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).expect("nonzero input id"))
}

fn receipt(value: u128) -> ActionReceiptId {
    ActionReceiptId::new(NonZeroU128::new(value).expect("nonzero receipt id"))
}

fn timecode(hours: u8, minutes: u8, seconds: u8, frames: u8) -> Timecode {
    Timecode::new(hours, minutes, seconds, frames, 30).expect("valid test timecode")
}

fn tap(recorder: u128, video_input: u128) -> IsoTapConfig {
    IsoTapConfig {
        recorder: id(recorder),
        video_input: input(video_input),
        audio_input: None,
        timecode_source: TimecodeSource::SharedMaster,
        source_timecode: None,
        segments: SegmentPolicy::default(),
        queue: QueueLimits::new(8, 1024 * 1024),
    }
}

fn source_tap(recorder: u128, video_input: u128, start_timecode: Timecode) -> IsoTapConfig {
    let mut tap = tap(recorder, video_input);
    tap.timecode_source = TimecodeSource::SourceOriginal;
    tap.source_timecode = Some(start_timecode);
    tap
}

fn timing(sequence: u64) -> MediaTiming {
    let time_base = TimeBase::new(1, 30).expect("valid time base");
    MediaTiming::new(
        OriginalTimestamp::new(
            MediaTimestamp::new(i64::try_from(sequence).expect("small sequence")),
            time_base,
        ),
        NormalizedTimestamp::from_nanos(
            i64::try_from(sequence * 33_333_333).expect("small timestamp"),
        ),
        NormalizedDuration::from_nanos(33_333_333).expect("nonzero duration"),
        ClockDomainId::new(NonZeroU128::new(1).expect("nonzero clock")),
        SequenceNumber::new(sequence),
    )
    .expect("valid timing")
}

fn video(sequence: u64, payload_len: usize) -> RecordEvent {
    let metadata = EncodedPacketMetadata::new(
        CodecId::new("video/h264").expect("valid codec"),
        CodecConfigGeneration::new(NonZeroU64::new(1).expect("nonzero generation")),
        StreamId::new(NonZeroU32::new(1).expect("nonzero stream")),
        None,
        timing(sequence),
        OriginalTimestamp::new(
            MediaTimestamp::new(i64::try_from(sequence).expect("small sequence")),
            TimeBase::new(1, 30).expect("valid time base"),
        ),
        PacketFlags::RANDOM_ACCESS,
    )
    .expect("matching timestamps");
    let packet = EncodedPacket::from_bytes(
        metadata,
        vec![u8::try_from(sequence).unwrap_or(255); payload_len],
    )
    .expect("nonempty bounded payload");
    RecordEvent::video(
        packet,
        VideoDimensions::new(1920, 1080).expect("valid dimensions"),
        PixelFormat::Nv12,
    )
    .expect("byte-backed packet")
}

fn recording_directory(root: &Path, recorder: RecorderId) -> PathBuf {
    root.join(format!("recorder-{:032x}", recorder.get().get()))
}

fn sorted_entries(path: &Path) -> Vec<PathBuf> {
    let mut entries = fs::read_dir(path)
        .expect("read directory")
        .map(|entry| entry.expect("directory entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn tap_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .skip(1)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn set_tap_field(text: &str, tap_index: usize, field_index: usize, value: &str) -> String {
    let mut lines = text.split('\n').map(str::to_owned).collect::<Vec<_>>();
    if let Some(line) = lines.get_mut(tap_index + 1) {
        let mut fields = line.split('\t').map(str::to_owned).collect::<Vec<_>>();
        if let Some(field) = fields.get_mut(field_index) {
            value.clone_into(field);
        }
        *line = fields.join("\t");
    }
    lines.join("\n")
}

fn drop_last_field(text: &str, tap_index: usize) -> String {
    let mut lines = text.split('\n').map(str::to_owned).collect::<Vec<_>>();
    if let Some(line) = lines.get_mut(tap_index + 1)
        && let Some((head, _tail)) = line.rsplit_once('\t')
    {
        *line = head.to_owned();
    }
    lines.join("\n")
}

fn push_field(text: &str, tap_index: usize, value: &str) -> String {
    let mut lines = text.split('\n').map(str::to_owned).collect::<Vec<_>>();
    if let Some(line) = lines.get_mut(tap_index + 1) {
        line.push('\t');
        line.push_str(value);
    }
    lines.join("\n")
}

fn push_line(text: &str, line: &str) -> String {
    format!("{text}{line}\n")
}

fn valid_manifest_text() -> String {
    let temp = TempDirectory::new("manifest-base");
    let root = temp.path().join("root");
    let mut first = source_tap(1, 101, timecode(1, 2, 3, 4));
    first.audio_input = Some(input(201));
    let second = tap(2, 102);
    let (session, _) = IsoTapSession::start(&root, Some(timecode(0, 0, 0, 0)), &[first, second])
        .expect("start manifest base");
    session.render_iso_manifest()
}

fn start_error(root: &Path, master: Option<Timecode>, taps: &[IsoTapConfig]) -> IsoTapError {
    match IsoTapSession::start(root, master, taps) {
        Ok((_, outcome)) => panic!("expected ISO start to fail, got {outcome:?}"),
        Err(error) => error,
    }
}

#[test]
fn starting_three_taps_writes_directories_and_manifest() {
    let temp = TempDirectory::new("three-taps");
    let root = temp.path().join("root");
    let master = timecode(20, 0, 0, 0);
    let taps = [tap(1, 101), tap(2, 102), tap(3, 103)];

    let (session, outcome) =
        IsoTapSession::start(&root, Some(master), &taps).expect("start three ISO taps");
    assert_eq!(outcome, ActionOutcome::Applied);

    let entries = sorted_entries(&root);
    assert_eq!(
        entries.len(),
        4,
        "three recorder directories and one manifest"
    );
    let directories = entries.iter().filter(|entry| entry.is_dir()).count();
    let files = entries
        .iter()
        .filter(|entry| entry.is_file())
        .map(|entry| entry.file_name().expect("file name").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(directories, 3);
    assert_eq!(files, vec![OsString::from(ISO_MANIFEST_NAME)]);
    for tap in &taps {
        assert!(recording_directory(&root, tap.recorder).is_dir());
    }

    let manifest = fs::read_to_string(root.join(ISO_MANIFEST_NAME)).expect("read ISO manifest");
    assert_eq!(manifest, session.render_iso_manifest());

    let snapshots = session.tap_snapshots();
    assert_eq!(
        snapshots
            .iter()
            .map(IsoTapSnapshot::slot)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    for (snapshot, tap) in snapshots.iter().zip(&taps) {
        assert_eq!(snapshot.config(), tap);
        assert_eq!(snapshot.snapshot().id, tap.recorder);
        assert_eq!(snapshot.snapshot().state, RecorderState::Recording);
    }
    assert_eq!(session.coordinator().snapshots().len(), 3);
    assert_eq!(session.timecode_for(1), Some(master));
    assert_eq!(session.timecode_for(2), Some(master));
    assert_eq!(session.timecode_for(3), Some(master));
}

#[test]
fn invalid_rosters_are_rejected_before_touching_the_root() {
    let temp = TempDirectory::new("invalid-rosters");

    let root = temp.path().join("duplicate-video");
    let error = start_error(&root, None, &[tap(1, 101), tap(2, 101)]);
    assert!(matches!(
        error,
        IsoTapError::DuplicateVideoInput(duplicate) if duplicate == input(101)
    ));
    assert!(!root.exists(), "a rejected roster must not create the root");

    let root = temp.path().join("duplicate-recorder");
    let error = start_error(&root, None, &[tap(1, 101), tap(1, 102)]);
    assert!(matches!(
        error,
        IsoTapError::DuplicateRecorder(recorder) if recorder == id(1)
    ));
    assert!(!root.exists(), "a rejected roster must not create the root");

    let root = temp.path().join("too-many-taps");
    let too_many = (0..=MAX_ISO_TAPS)
        .map(|index| {
            let value = u128::try_from(index).expect("small index") + 1;
            tap(value, value + 1000)
        })
        .collect::<Vec<_>>();
    let error = start_error(&root, None, &too_many);
    assert!(matches!(error, IsoTapError::InvalidTapCount { count } if count == MAX_ISO_TAPS + 1));
    assert!(!root.exists(), "a rejected roster must not create the root");

    let root = temp.path().join("zero-taps");
    let error = start_error(&root, None, &[]);
    assert!(matches!(error, IsoTapError::InvalidTapCount { count: 0 }));
    assert!(!root.exists(), "a rejected roster must not create the root");

    let root = temp.path().join("missing-source-timecode");
    let mut missing = tap(1, 101);
    missing.timecode_source = TimecodeSource::SourceOriginal;
    let error = start_error(&root, None, &[missing]);
    assert!(matches!(
        error,
        IsoTapError::MissingSourceTimecode { slot: 1 }
    ));
    assert!(!root.exists(), "a rejected roster must not create the root");

    let root = temp.path().join("unexpected-source-timecode");
    let mut unexpected = tap(1, 101);
    unexpected.source_timecode = Some(timecode(0, 0, 0, 0));
    let error = start_error(&root, None, &[unexpected]);
    assert!(matches!(
        error,
        IsoTapError::UnexpectedSourceTimecode { slot: 1 }
    ));
    assert!(!root.exists(), "a rejected roster must not create the root");

    let root = temp.path().join("invalid-config");
    let mut invalid = tap(1, 101);
    invalid.queue = QueueLimits::new(0, 1);
    let error = start_error(&root, None, &[invalid]);
    assert!(matches!(error, IsoTapError::InvalidConfig { slot: 1, .. }));
    assert!(!root.exists(), "a rejected roster must not create the root");
}

#[test]
fn later_start_failure_rolls_back_started_taps() {
    let temp = TempDirectory::new("start-rollback");
    let root = temp.path().join("root");
    fs::create_dir_all(&root).expect("create session root");
    let blocked = id(2);
    fs::write(
        recording_directory(&root, blocked),
        b"blocked recorder path",
    )
    .expect("write blocking file");

    let error = start_error(&root, None, &[tap(1, 101), tap(2, 102)]);
    match error {
        IsoTapError::Start { slot: 2, source } => {
            assert!(
                matches!(source, RecorderError::Io { .. }),
                "unexpected start failure: {source}"
            );
        }
        other => panic!("unexpected error: {other}"),
    }
    assert!(
        !root.join(ISO_MANIFEST_NAME).exists(),
        "a failed start must not publish a manifest"
    );

    let (reopened, report) = RecorderCoordinator::open(&root).expect("reopen session root");
    assert!(report.failures.is_empty());
    assert_eq!(
        reopened.snapshot(id(1)).expect("first tap snapshot").state,
        RecorderState::Stopped,
        "a rolled-back tap must not stay live"
    );
}

#[test]
fn record_flush_and_stop_route_to_the_addressed_tap() {
    let temp = TempDirectory::new("routing");
    let root = temp.path().join("root");
    let (mut session, _) =
        IsoTapSession::start(&root, None, &[tap(1, 101), tap(2, 102)]).expect("start session");

    session.record(1, video(0, 16)).expect("record to slot 1");
    let untouched = session.tap_snapshots()[1].snapshot().clone();
    assert_eq!((untouched.queued_events, untouched.written_frames), (0, 0));

    assert_eq!(session.flush(1).expect("flush slot 1"), 1);
    let first = session.tap_snapshots()[0].snapshot().clone();
    assert_eq!(first.written_frames, 1);
    assert!(first.written_bytes > 0);
    let second = session.tap_snapshots()[1].snapshot().clone();
    assert_eq!((second.queued_events, second.written_frames), (0, 0));

    session.record(2, video(1, 16)).expect("record to slot 2");
    assert_eq!(session.tap_snapshots()[1].snapshot().queued_events, 1);
    assert_eq!(session.flush_all().expect("flush both taps"), 1);
    assert_eq!(session.tap_snapshots()[1].snapshot().written_frames, 1);
    assert_eq!(session.tap_snapshots()[0].snapshot().written_frames, 1);

    assert_eq!(
        session.stop(1, receipt(1)).expect("stop slot 1"),
        ActionOutcome::Applied
    );
    assert_eq!(
        session.tap_snapshots()[0].snapshot().state,
        RecorderState::Stopped
    );
    assert_eq!(
        session.tap_snapshots()[1].snapshot().state,
        RecorderState::Recording
    );

    let rejected = session
        .record(1, video(2, 16))
        .expect_err("a stopped tap rejects events");
    assert!(matches!(
        rejected,
        IsoTapError::Enqueue {
            slot: 1,
            source: EnqueueError::NotRecording { .. }
        }
    ));
    assert!(matches!(
        session.flush(1),
        Err(IsoTapError::Recorder { slot: 1, .. })
    ));
    session
        .record(2, video(3, 16))
        .expect("the other tap still records");

    assert_eq!(
        session.stop_all().expect("stop remaining taps"),
        ActionOutcome::Applied
    );
    assert_eq!(
        session.tap_snapshots()[1].snapshot().state,
        RecorderState::Stopped
    );
    assert_eq!(
        session.stop_all().expect("repeat stop all"),
        ActionOutcome::AlreadyApplied
    );
}

#[test]
fn unknown_slots_are_rejected() {
    let temp = TempDirectory::new("unknown-slots");
    let root = temp.path().join("root");
    let (mut session, _) =
        IsoTapSession::start(&root, None, &[tap(1, 101), tap(2, 102)]).expect("start session");

    assert!(matches!(
        session.record(0, video(0, 8)),
        Err(IsoTapError::UnknownSlot { slot: 0 })
    ));
    assert!(matches!(
        session.record(3, video(0, 8)),
        Err(IsoTapError::UnknownSlot { slot: 3 })
    ));
    assert!(matches!(
        session.flush(0),
        Err(IsoTapError::UnknownSlot { slot: 0 })
    ));
    assert!(matches!(
        session.flush(u8::try_from(MAX_ISO_TAPS).expect("small bound") + 1),
        Err(IsoTapError::UnknownSlot { slot: 9 })
    ));
    assert!(matches!(
        session.stop(0, receipt(1)),
        Err(IsoTapError::UnknownSlot { slot: 0 })
    ));
    assert!(matches!(
        session.stop(9, receipt(2)),
        Err(IsoTapError::UnknownSlot { slot: 9 })
    ));
    assert_eq!(session.timecode_for(0), None);
    assert_eq!(session.timecode_for(3), None);
    assert_eq!(session.flush_all().expect("both slots still flush"), 0);
}

#[test]
fn manifest_round_trip_is_exact_for_one_and_eight_taps() {
    let master = timecode(1, 2, 3, 4);
    for count in [1usize, MAX_ISO_TAPS] {
        let temp = TempDirectory::new(&format!("round-trip-{count}"));
        let root = temp.path().join("root");
        let taps = (0..count)
            .map(|index| {
                let value = u128::try_from(index).expect("small index") + 1;
                let mut tap = tap(value, value + 100);
                tap.queue = QueueLimits::new(4, 2048);
                match index {
                    0 => tap.segments = SegmentPolicy::by_frames(2),
                    1 => tap.segments = SegmentPolicy::by_bytes(4096),
                    2 => {
                        tap.segments = SegmentPolicy::by_duration(
                            NormalizedDuration::from_nanos(1_000_000).expect("nonzero duration"),
                        );
                    }
                    _ => {}
                }
                if index % 2 == 0 {
                    let hours = u8::try_from(index).expect("small index");
                    tap.timecode_source = TimecodeSource::SourceOriginal;
                    tap.source_timecode = Some(timecode(hours, 1, 2, 3));
                } else {
                    tap.audio_input = Some(input(value + 200));
                }
                tap
            })
            .collect::<Vec<_>>();
        let (session, _) = IsoTapSession::start(&root, Some(master), &taps).expect("start taps");

        let text = session.render_iso_manifest();
        let entries = parse_iso_manifest(&text).expect("parse manifest");
        assert_eq!(entries.len(), count);
        for (index, (entry, tap)) in entries.iter().zip(&taps).enumerate() {
            assert_eq!(usize::from(entry.slot()), index + 1);
            assert_eq!(entry.recorder(), tap.recorder);
            assert_eq!(entry.video_input(), tap.video_input);
            assert_eq!(entry.audio_input(), tap.audio_input);
            assert_eq!(entry.timecode_source(), tap.timecode_source);
            assert_eq!(entry.start_timecode(), tap.source_timecode.or(Some(master)));
            assert_eq!(entry.segments(), tap.segments);
            assert_eq!(entry.queue(), tap.queue);
        }
        assert_eq!(render_iso_manifest_entries(&entries), text);
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn malformed_manifests_are_rejected_wholesale() {
    let base = valid_manifest_text();
    let header = "fm-record-iso\t1";
    assert!(base.starts_with("fm-record-iso\t1\n"));

    let bad_magic = base.replacen(header, "fm-record-iso!\t1", 1);
    assert!(matches!(
        parse_iso_manifest(&bad_magic),
        Err(IsoManifestError::InvalidHeader)
    ));

    let bad_version = base.replacen(header, "fm-record-iso\t2", 1);
    assert!(matches!(
        parse_iso_manifest(&bad_version),
        Err(IsoManifestError::UnsupportedVersion { version: 2 })
    ));

    let missing_header = base.split_once('\n').expect("header line").1.to_owned();
    assert!(matches!(
        parse_iso_manifest(&missing_header),
        Err(IsoManifestError::InvalidHeader)
    ));

    let duplicate_slot = set_tap_field(&base, 1, 0, "1");
    assert!(matches!(
        parse_iso_manifest(&duplicate_slot),
        Err(IsoManifestError::DuplicateSlot { slot: 1 })
    ));

    let lines = tap_lines(&base);
    let out_of_order = format!("{header}\n{}\n{}\n", lines[1], lines[0]);
    assert!(matches!(
        parse_iso_manifest(&out_of_order),
        Err(IsoManifestError::SlotsOutOfOrder {
            line: 3,
            previous: 2,
            slot: 1
        })
    ));

    let duplicate_video = set_tap_field(&base, 1, 2, "101");
    assert!(matches!(
        parse_iso_manifest(&duplicate_video),
        Err(IsoManifestError::DuplicateVideoInput { video_input }) if video_input == input(101)
    ));

    let duplicate_recorder = set_tap_field(&base, 1, 1, "1");
    assert!(matches!(
        parse_iso_manifest(&duplicate_recorder),
        Err(IsoManifestError::DuplicateRecorder { recorder }) if recorder == id(1)
    ));

    let unknown_source = set_tap_field(&base, 0, 4, "master");
    assert!(matches!(
        parse_iso_manifest(&unknown_source),
        Err(IsoManifestError::UnknownTimecodeSource { line: 2 })
    ));

    for value in ["24:00:00:00", "00:00:00", "00:00:00:0x", "1:02:03:04"] {
        let bad_timecode = set_tap_field(&base, 0, 5, value);
        assert!(
            matches!(
                parse_iso_manifest(&bad_timecode),
                Err(IsoManifestError::MalformedField {
                    line: 2,
                    field: "start_timecode"
                })
            ),
            "timecode {value} must be rejected"
        );
    }

    let missing_timecode = set_tap_field(&base, 0, 5, "none");
    assert!(matches!(
        parse_iso_manifest(&missing_timecode),
        Err(IsoManifestError::MissingSourceTimecode { line: 2 })
    ));

    let malformed_id = set_tap_field(&base, 0, 1, "not-a-number");
    assert!(matches!(
        parse_iso_manifest(&malformed_id),
        Err(IsoManifestError::MalformedField {
            line: 2,
            field: "recorder"
        })
    ));
    let zero_id = set_tap_field(&base, 0, 1, "0");
    assert!(matches!(
        parse_iso_manifest(&zero_id),
        Err(IsoManifestError::ZeroIdentifier {
            line: 2,
            field: "recorder"
        })
    ));

    for value in ["0", "9", "256"] {
        let bad_slot = set_tap_field(&base, 0, 0, value);
        assert!(
            matches!(
                parse_iso_manifest(&bad_slot),
                Err(IsoManifestError::SlotOutOfRange { line: 2, .. })
            ),
            "slot {value} must be rejected"
        );
    }

    let zero_bound = set_tap_field(&base, 1, 10, "0");
    assert!(matches!(
        parse_iso_manifest(&zero_bound),
        Err(IsoManifestError::InvalidBound {
            line: 3,
            field: "queue_max_bytes"
        })
    ));

    let missing_field = drop_last_field(&base, 0);
    assert!(matches!(
        parse_iso_manifest(&missing_field),
        Err(IsoManifestError::FieldCount { line: 2, count: 10 })
    ));
    let extra_field = push_field(&base, 0, "extra");
    assert!(matches!(
        parse_iso_manifest(&extra_field),
        Err(IsoManifestError::FieldCount { line: 2, count: 12 })
    ));

    let mut too_many = base.clone();
    for _ in 0..7 {
        too_many = push_line(&too_many, &lines[0]);
    }
    assert_eq!(tap_lines(&too_many).len(), MAX_ISO_TAPS + 1);
    assert!(matches!(
        parse_iso_manifest(&too_many),
        Err(IsoManifestError::TooManyLines { lines }) if lines == MAX_ISO_TAPS + 1
    ));

    let oversized = push_line(&base, &"a".repeat(600));
    assert!(matches!(
        parse_iso_manifest(&oversized),
        Err(IsoManifestError::LineTooLong { line: 4 })
    ));
}

#[test]
fn high_frame_rate_timecodes_round_trip_through_the_persisted_manifest() {
    let temp = TempDirectory::new("high-frame-rate-timecode");
    let root = temp.path().join("root");
    let source = Timecode::new(1, 2, 3, 119, 120).expect("valid 120 fps timecode");
    let master = Timecode::new(4, 5, 6, 100, 120).expect("valid 120 fps timecode");
    let (session, _) = IsoTapSession::start(
        &root,
        Some(master),
        &[source_tap(1, 101, source), tap(2, 102)],
    )
    .expect("start high frame rate taps");

    let persisted = fs::read_to_string(root.join(ISO_MANIFEST_NAME)).expect("read manifest");
    let entries = parse_iso_manifest(&persisted).expect("session manifest must parse");
    assert_eq!(entries[0].start_timecode(), Some(source));
    assert_eq!(entries[1].start_timecode(), Some(master));
    assert_eq!(render_iso_manifest_entries(&entries), persisted);
    drop(session);

    for value in ["01:02:03:099", "01:02:03:0119", "01:02:03:1"] {
        let text = persisted.replacen("01:02:03:119", value, 1);
        assert!(
            matches!(
                parse_iso_manifest(&text),
                Err(IsoManifestError::MalformedField {
                    line: 2,
                    field: "start_timecode"
                })
            ),
            "non-canonical timecode {value} must be rejected"
        );
    }
}

#[test]
fn shared_and_source_timecodes_resolve_exactly() {
    let temp = TempDirectory::new("timecodes");
    let root = temp.path().join("root");
    let master = timecode(12, 34, 56, 7);
    let first = source_tap(1, 101, timecode(0, 0, 0, 0));
    let last = source_tap(2, 102, timecode(23, 59, 59, 24));
    let mut shared = tap(3, 103);
    shared.audio_input = Some(input(201));
    let (session, _) = IsoTapSession::start(&root, Some(master), &[first, last, shared])
        .expect("start mixed timecode taps");

    assert_eq!(session.timecode_for(1), Some(timecode(0, 0, 0, 0)));
    assert_eq!(session.timecode_for(2), Some(timecode(23, 59, 59, 24)));
    assert_eq!(session.timecode_for(3), Some(master));

    let text = session.render_iso_manifest();
    for expected in ["00:00:00:00", "23:59:59:24", "12:34:56:07"] {
        assert!(text.contains(expected), "manifest must carry {expected}");
    }
    let entries = parse_iso_manifest(&text).expect("parse manifest");
    assert_eq!(entries[0].timecode_source(), TimecodeSource::SourceOriginal);
    assert_eq!(entries[0].start_timecode(), Some(timecode(0, 0, 0, 0)));
    assert_eq!(entries[1].start_timecode(), Some(timecode(23, 59, 59, 24)));
    assert_eq!(entries[2].timecode_source(), TimecodeSource::SharedMaster);
    assert_eq!(entries[2].start_timecode(), Some(master));
    assert_eq!(entries[2].audio_input(), Some(input(201)));
    assert_eq!(
        session.tap_snapshots()[2]
            .snapshot()
            .tap
            .expect("tap metadata")
            .start_timecode,
        Some(master)
    );

    let root = temp.path().join("shared-without-master");
    let (session, _) =
        IsoTapSession::start(&root, None, &[tap(4, 104)]).expect("start without master");
    assert_eq!(session.timecode_for(1), None);
    let entries = parse_iso_manifest(&session.render_iso_manifest()).expect("parse manifest");
    assert_eq!(entries[0].start_timecode(), None);

    let root = temp.path().join("source-without-master");
    let source = source_tap(5, 105, timecode(1, 0, 0, 0));
    let (session, _) =
        IsoTapSession::start(&root, None, &[source]).expect("source timecode without master");
    assert_eq!(session.timecode_for(1), Some(timecode(1, 0, 0, 0)));
}

#[test]
fn already_applied_start_is_idempotent() {
    let temp = TempDirectory::new("already-applied");
    let root = temp.path().join("root");
    let taps = [tap(1, 101), tap(2, 102)];
    let (first, outcome) =
        IsoTapSession::start(&root, Some(timecode(1, 0, 0, 0)), &taps).expect("first start");
    assert_eq!(outcome, ActionOutcome::Applied);
    let manifest = first.render_iso_manifest();
    drop(first);

    let (mut second, outcome) =
        IsoTapSession::start(&root, Some(timecode(1, 0, 0, 0)), &taps).expect("repeat start");
    assert_eq!(outcome, ActionOutcome::AlreadyApplied);
    assert_eq!(second.render_iso_manifest(), manifest);
    assert_eq!(
        second.tap_snapshots()[0].snapshot().state,
        RecorderState::Recording
    );

    second
        .record(1, video(0, 16))
        .expect("record into open tap");
    assert_eq!(second.flush(1).expect("flush open tap"), 1);
    drop(second);

    let (reopened, report) = RecorderCoordinator::open(&root).expect("reopen root");
    assert!(report.failures.is_empty());
    let snapshot = reopened.snapshot(id(1)).expect("recovered snapshot");
    assert_eq!(snapshot.state, RecorderState::Recording);
    assert_eq!(snapshot.written_frames, 1);
}

#[test]
fn mixed_start_receipts_are_rejected_and_rolled_back() {
    let temp = TempDirectory::new("mixed-receipts");
    let root = temp.path().join("root");
    let (mut first, _) = IsoTapSession::start(&root, None, &[tap(1, 101)]).expect("first start");
    assert_eq!(
        first.stop_all().expect("stop first session"),
        ActionOutcome::Applied
    );
    drop(first);

    let error = start_error(&root, None, &[tap(1, 101), tap(2, 102)]);
    assert!(matches!(error, IsoTapError::MixedStartOutcome));

    let (reopened, report) = RecorderCoordinator::open(&root).expect("reopen root");
    assert!(report.failures.is_empty());
    assert_eq!(
        reopened.snapshot(id(1)).expect("first tap").state,
        RecorderState::Stopped
    );
    assert_eq!(
        reopened.snapshot(id(2)).expect("second tap").state,
        RecorderState::Stopped
    );
}
