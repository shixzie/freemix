//! OR-012 rolling replay storage: one fragmented MP4 stream cut into
//! independently playable segment files with exact frame counts, open
//! segments that grow with the live edge, and time-based retention that
//! deletes old segments but never those an event protects.
//!
//! The stream is built from raw ISO BMFF boxes (one video and one audio
//! track, 25 video frames per fragment), so every count is exact.

use std::{
    collections::BTreeSet,
    env, fs,
    num::{NonZeroU32, NonZeroU128},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use fm_frame::NormalizedTimestamp;
use fm_record::{Fmp4Integrity, scan_fmp4_file};
use fm_replay::{
    CameraId, DEFAULT_SEGMENT_UNIT_BYTES, EventId, ProtectionReference, ReplayError,
    ReplaySegmentWriter, RollingSegmentCatalog, SegmentFileEvent, SegmentId, SegmentMetadata,
    SegmentState, SegmentWriterConfig, StorageRoot, StorageRootId, TimelineRange,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_nanos();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "fm-replay-or012-{}-{nonce}-{counter}",
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

#[derive(Clone, Copy)]
struct TrackSpec {
    id: u32,
    timescale: u32,
    handler: [u8; 4],
    sample_duration: u32,
    sample_size: u32,
    samples: u32,
}

const VIDEO: TrackSpec = TrackSpec {
    id: 1,
    timescale: 90_000,
    handler: *b"vide",
    sample_duration: 3_600,
    sample_size: 40,
    samples: 25,
};

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

fn init_segment(tracks: &[TrackSpec]) -> Vec<u8> {
    let ftyp = mp4_box(*b"ftyp", b"isom\0\0\x02\0isomiso6");
    let mut moov = full_box(*b"mvhd", 0, 0, &[0u8; 96]);
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
    cat(&[&ftyp, &mp4_box(*b"moov", &moov)])
}

fn traf(track: &TrackSpec, decode_time: u64, data_offset: i32) -> Vec<u8> {
    let tfhd = full_box(*b"tfhd", 0, 0x2_0000, &track.id.to_be_bytes());
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
fn fragment(number: u32) -> Vec<u8> {
    let build = |offsets: &[i32]| {
        let mut moof = full_box(*b"mfhd", 0, 0, &number.to_be_bytes());
        for (track, offset) in TRACKS.iter().zip(offsets) {
            let decode = u64::from(number - 1) * u64::from(track.samples * track.sample_duration);
            moof.extend(traf(track, decode, *offset));
        }
        mp4_box(*b"moof", &moof)
    };
    let moof_len = build(&[0; TRACKS.len()]).len();
    let mut offsets = Vec::new();
    let mut cursor = moof_len + 8;
    for track in &TRACKS {
        offsets.push(i32::try_from(cursor).unwrap());
        cursor += data_len(track);
    }
    let moof = build(&offsets);
    let mut data = Vec::new();
    for track in &TRACKS {
        data.extend(std::iter::repeat_n(
            number.to_le_bytes()[0],
            data_len(track),
        ));
    }
    cat(&[&moof, &mp4_box(*b"mdat", &data)])
}

fn writer(directory: &Path, fragments: u32) -> ReplaySegmentWriter {
    ReplaySegmentWriter::new(SegmentWriterConfig {
        directory: directory.to_path_buf(),
        prefix: "angle-7".to_owned(),
        fragments_per_segment: NonZeroU32::new(fragments).unwrap(),
        max_unit_bytes: DEFAULT_SEGMENT_UNIT_BYTES,
    })
}

#[test]
fn a_stream_is_cut_into_playable_segments_with_exact_frame_counts() {
    let directory = TempDirectory::new();
    let mut writer = writer(directory.path(), 2);
    let mut stream = init_segment(&TRACKS);
    for number in 1..=5 {
        stream.extend(fragment(number));
    }
    // Any chunking yields the same files.
    let mut events = Vec::new();
    for chunk in stream.chunks(97) {
        events.extend(writer.push(chunk).expect("valid stream"));
    }
    events.extend(writer.finish().expect("clean end"));

    let path = |index| directory.path().join(format!("angle-7-{index:06}.mp4"));
    let closed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SegmentFileEvent::Closed {
                index,
                video_frames,
                ..
            } => Some((*index, *video_frames)),
            SegmentFileEvent::Opened { .. } => None,
        })
        .collect();
    assert_eq!(closed, [(1, 50), (2, 50), (3, 25)]);
    // Each segment opens before it closes, and the next opens right after.
    assert_eq!(
        events[0],
        SegmentFileEvent::Opened {
            index: 1,
            path: path(1)
        }
    );
    assert!(matches!(
        events[1],
        SegmentFileEvent::Closed { index: 1, .. }
    ));
    assert_eq!(
        events[2],
        SegmentFileEvent::Opened {
            index: 2,
            path: path(2)
        }
    );
    for (index, frames) in [(1, 50), (2, 50), (3, 25)] {
        let scan = scan_fmp4_file(&path(index)).expect("segment scans");
        assert_eq!(scan.integrity, Fmp4Integrity::Complete, "segment {index}");
        let video = scan
            .spans
            .iter()
            .find(|span| span.track_id == VIDEO.id)
            .expect("video span");
        assert_eq!(video.samples, frames, "segment {index}");
        let SegmentFileEvent::Closed { bytes, .. } = events
            .iter()
            .find(|event| matches!(event, SegmentFileEvent::Closed { index: closed, .. } if *closed == index))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(fs::metadata(path(index)).unwrap().len(), *bytes);
    }
}

#[test]
fn a_stream_without_video_or_with_garbage_is_refused_and_stays_refused() {
    let directory = TempDirectory::new();
    let mut audio_only = writer(directory.path(), 2);
    let mut stream = init_segment(&[AUDIO]);
    stream.extend(mp4_box(
        *b"moof",
        &full_box(*b"mfhd", 0, 0, &1u32.to_be_bytes()),
    ));
    assert!(audio_only.push(&stream).is_err());
    assert!(audio_only.push(&fragment(1)).is_err());

    let mut garbage = writer(directory.path(), 2);
    assert!(garbage.push(&mp4_box(*b"junk", &[0; 16])).is_err());
    assert!(fs::read_dir(directory.path()).unwrap().next().is_none());
}

fn camera(value: u128) -> CameraId {
    CameraId::new(NonZeroU128::new(value).unwrap())
}

fn root_id() -> StorageRootId {
    StorageRootId::new(NonZeroU128::MIN)
}

fn segment_id(value: u128) -> SegmentId {
    SegmentId::new(NonZeroU128::new(value).unwrap())
}

fn ts(seconds: i64) -> NormalizedTimestamp {
    NormalizedTimestamp::from_nanos(seconds * 1_000_000_000)
}

fn open_segment(id: u128, start: i64) -> SegmentMetadata {
    SegmentMetadata {
        id: segment_id(id),
        camera_id: camera(1),
        storage_root_id: root_id(),
        recorder_segment_index: u64::try_from(id).unwrap(),
        timeline: TimelineRange::new(ts(start), ts(start + 1)).unwrap(),
        path: PathBuf::from(format!("segment-{id}.mp4")),
        bytes: 0,
        state: SegmentState::Open,
        protections: BTreeSet::new(),
    }
}

fn catalog() -> RollingSegmentCatalog {
    let mut catalog = RollingSegmentCatalog::default();
    catalog
        .add_root(StorageRoot {
            id: root_id(),
            path: PathBuf::from("replay"),
            capacity_bytes: 0,
        })
        .unwrap();
    catalog.assign_camera(camera(1), root_id()).unwrap();
    catalog
}

#[test]
fn open_segments_follow_the_live_edge_and_close_exactly() {
    let mut catalog = catalog();
    catalog.insert_segment(open_segment(1, 0)).unwrap();
    catalog.extend_open_segment(segment_id(1), ts(2)).unwrap();
    assert_eq!(catalog.segment(segment_id(1)).unwrap().timeline.end, ts(2));
    assert_eq!(
        catalog.extend_open_segment(segment_id(1), ts(1)),
        Err(ReplayError::InvalidRange)
    );
    // The live edge is covered while the segment is still open.
    assert!(
        catalog
            .coverage(camera(1), TimelineRange::new(ts(0), ts(2)).unwrap())
            .is_complete()
    );
    catalog.close_segment(segment_id(1), ts(2), 4_096).unwrap();
    let closed = catalog.segment(segment_id(1)).unwrap();
    assert_eq!(closed.state, SegmentState::Closed);
    assert_eq!(closed.bytes, 4_096);
    assert_eq!(
        catalog.extend_open_segment(segment_id(1), ts(3)),
        Err(ReplayError::SegmentClosed(segment_id(1)))
    );
    assert_eq!(
        catalog.close_segment(segment_id(9), ts(3), 1),
        Err(ReplayError::UnknownSegment(segment_id(9)))
    );
}

#[test]
fn time_retention_deletes_old_closed_segments_but_not_protected_or_open_ones() {
    let mut catalog = catalog();
    for id in 1..=5_i64 {
        let start = (id - 1) * 2;
        let value = u128::try_from(id).unwrap();
        catalog.insert_segment(open_segment(value, start)).unwrap();
        if id < 5 {
            catalog
                .close_segment(segment_id(value), ts(start + 2), 100)
                .unwrap();
        }
    }
    // An event over 2..3 s protects segment 2.
    let event = ProtectionReference::Event(EventId::new(NonZeroU128::MIN));
    let protected =
        catalog.protect_range(camera(1), TimelineRange::new(ts(2), ts(3)).unwrap(), event);
    assert_eq!(protected, [segment_id(2)]);

    let removed = catalog.reclaim_ended_before(root_id(), ts(8)).unwrap();
    let removed: Vec<_> = removed.iter().map(|segment| segment.id).collect();
    // Segments 1, 3, and 4 end by 8 s; 2 is protected and 5 is open.
    assert_eq!(removed, [segment_id(1), segment_id(3), segment_id(4)]);
    let left: Vec<_> = catalog.segments().map(|segment| segment.id).collect();
    assert_eq!(left, [segment_id(2), segment_id(5)]);

    // Releasing the event lets the next pass delete it.
    catalog.release_reference(event);
    let removed = catalog.reclaim_ended_before(root_id(), ts(8)).unwrap();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].id, segment_id(2));
    assert_eq!(
        catalog
            .remove_camera_segments(camera(1))
            .iter()
            .map(|segment| segment.id)
            .collect::<Vec<_>>(),
        [segment_id(5)]
    );
}
