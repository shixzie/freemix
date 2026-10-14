//! Event database capture against the rolling segment catalog, synchronized
//! multi-angle source-time resolution, and Replay A/B frame resolution.

use std::{collections::BTreeSet, num::NonZeroU128, path::PathBuf};

use fm_frame::{ClockDomainId, NormalizedTimestamp, SequenceNumber};
use fm_playback::FrameIndex;
use fm_record::RecorderId;
use fm_replay::{
    CameraId, CameraSource, ChannelMode, EventDatabase, EventFolder, EventId, EventList, FolderId,
    LastNPreset, ListId, MAX_LISTS, MAX_NAME_BYTES, MAX_NOTE_BYTES, MAX_TAGS_PER_EVENT,
    PlaybackRate, ProtectionReference, ReplayChannelId, ReplayDecks, ReplayError, ReplayEvent,
    ReplayLimit, RollingSegmentCatalog, SegmentId, SegmentMetadata, SegmentState, SourceCatalog,
    SourceTimeline, StorageRoot, StorageRootId, TimelineRange,
};
use fm_types::{Channel, ChannelLayout};

const SECOND: i64 = 1_000_000_000;
/// 50 fps frame cadence.
const FRAME: i64 = 20_000_000;
const CAMERAS: u128 = 4;

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn camera(value: u128) -> CameraId {
    CameraId::new(nonzero(value))
}

fn root(value: u128) -> StorageRootId {
    StorageRootId::new(nonzero(value))
}

fn event_id(value: u128) -> EventId {
    EventId::new(nonzero(value))
}

fn segment_id(camera_value: u128, index: u64) -> SegmentId {
    SegmentId::new(nonzero(camera_value * 100 + u128::from(index)))
}

fn ts(nanos: i64) -> NormalizedTimestamp {
    NormalizedTimestamp::from_nanos(nanos)
}

fn range(start: i64, end: i64) -> TimelineRange {
    TimelineRange::new(ts(start), ts(end)).unwrap()
}

/// Per-camera capture offset, modelling small synchronization skew.
fn skew(camera_value: u128) -> i64 {
    i64::try_from(camera_value).unwrap() * 1_000_000
}

fn source_catalog() -> SourceCatalog {
    SourceCatalog::new(
        (1..=CAMERAS)
            .map(|value| CameraSource {
                id: camera(value),
                name: format!("Camera {value}"),
                storage_root_id: root(value),
                recorder_id: RecorderId::new(nonzero(value)),
                clock_domain: ClockDomainId::new(nonzero(value)),
                audio_layout: ChannelLayout::new(vec![
                    Channel::Left,
                    Channel::Right,
                    Channel::Center,
                    Channel::LowFrequency,
                ])
                .unwrap(),
            })
            .collect(),
    )
    .unwrap()
}

/// Every camera records two closed one-second segments (0..1s, 1..2s) and
/// one open segment (2..3s) on its own storage root.
fn segment_catalog() -> RollingSegmentCatalog {
    let mut catalog = RollingSegmentCatalog::default();
    for value in 1..=CAMERAS {
        catalog
            .add_root(StorageRoot {
                id: root(value),
                path: PathBuf::from(format!("/replay/root-{value}")),
                capacity_bytes: 1_000_000,
            })
            .unwrap();
        catalog.assign_camera(camera(value), root(value)).unwrap();
        for index in 0..3_u64 {
            let start = i64::try_from(index).unwrap() * SECOND;
            catalog
                .insert_segment(SegmentMetadata {
                    id: segment_id(value, index),
                    camera_id: camera(value),
                    storage_root_id: root(value),
                    recorder_segment_index: index,
                    timeline: range(start, start + SECOND),
                    path: PathBuf::from(format!("/replay/root-{value}/{index}.seg")),
                    bytes: 1_000,
                    state: if index == 2 {
                        SegmentState::Open
                    } else {
                        SegmentState::Closed
                    },
                    protections: BTreeSet::new(),
                })
                .unwrap();
        }
    }
    catalog
}

/// 150 frames (3 s at 50 fps) per camera, offset by the camera skew.
fn source_timeline() -> SourceTimeline {
    let mut timeline = SourceTimeline::new(&source_catalog());
    for frame in 0..150_u64 {
        for value in 1..=CAMERAS {
            timeline
                .observe(
                    camera(value),
                    ts(i64::try_from(frame).unwrap() * FRAME + skew(value)),
                    SequenceNumber::new(frame + 1),
                    FrameIndex::new(frame),
                )
                .unwrap();
        }
    }
    timeline
}

fn replay_event(id: u128, timeline: TimelineRange, angles: &[u128]) -> ReplayEvent {
    ReplayEvent {
        id: event_id(id),
        name: format!("Event {id}"),
        timeline,
        angles: angles.iter().copied().map(camera).collect(),
        preferred_angle: camera(angles[0]),
        tags: BTreeSet::new(),
        note: String::new(),
        folders: BTreeSet::new(),
    }
}

#[test]
fn last_n_presets_end_exactly_at_the_live_edge() {
    let live_edge = ts(30 * SECOND);
    let ranges: Vec<_> = LastNPreset::ALL
        .iter()
        .map(|preset| preset.range(live_edge).unwrap())
        .collect();
    assert_eq!(
        ranges,
        vec![
            range(25 * SECOND, 30 * SECOND),
            range(20 * SECOND, 30 * SECOND),
            range(10 * SECOND, 30 * SECOND),
        ]
    );
    assert_eq!(
        LastNPreset::Last20.range(ts(i64::MIN + 1)),
        Err(ReplayError::InvalidLastN)
    );
}

#[test]
fn coverage_and_segment_lookup_are_deterministic() {
    let mut catalog = segment_catalog();
    let coverage = catalog.coverage(camera(2), range(SECOND / 2, 5 * SECOND / 2));
    assert!(coverage.is_complete());
    assert_eq!(
        coverage.segments,
        vec![segment_id(2, 0), segment_id(2, 1), segment_id(2, 2)]
    );
    assert_eq!(
        catalog.segment_at(camera(2), ts(SECOND)).unwrap().id,
        segment_id(2, 1)
    );
    assert!(catalog.segment_at(camera(2), ts(3 * SECOND)).is_none());

    let reclaimed = catalog.reclaim(root(2), 1_000).unwrap();
    assert_eq!(reclaimed.deleted, vec![segment_id(2, 0)]);
    let coverage = catalog.coverage(camera(2), range(SECOND / 2, 4 * SECOND));
    assert_eq!(
        coverage.gaps,
        vec![range(SECOND / 2, SECOND), range(3 * SECOND, 4 * SECOND)]
    );
    assert_eq!(coverage.segments, vec![segment_id(2, 1), segment_id(2, 2)]);
}

#[test]
fn source_resolution_is_frame_accurate_per_angle() {
    let timeline = source_timeline();
    // Camera 3 frames sit at n * 20 ms + 3 ms.
    let exact = timeline
        .resolve(camera(3), ts(10 * FRAME + 3_000_000))
        .unwrap();
    assert_eq!(exact.source_frame, FrameIndex::new(10));
    let between = timeline
        .resolve(camera(3), ts(11 * FRAME + 2_999_999))
        .unwrap();
    assert_eq!(between.source_frame, FrameIndex::new(10));
    assert_eq!(
        timeline.resolve(camera(3), ts(2_999_999)),
        Err(ReplayError::ObservationUnavailable(camera(3)))
    );
    assert_eq!(
        timeline.resolve(camera(9), ts(0)),
        Err(ReplayError::UnknownCamera(camera(9)))
    );

    // One second contains exactly 50 frames for every skewed angle.
    for value in 1..=CAMERAS {
        let span = timeline
            .frames_in(camera(value), range(SECOND, 2 * SECOND))
            .unwrap();
        assert_eq!(span.frame_count, 50);
        assert_eq!(span.first.source_frame, FrameIndex::new(50));
        assert_eq!(span.last.source_frame, FrameIndex::new(99));
        assert_eq!(span.discontinuities, 0);
    }
    assert_eq!(
        timeline.frames_in(camera(1), range(10 * SECOND, 11 * SECOND)),
        Err(ReplayError::NoFramesInRange(camera(1)))
    );
}

#[test]
fn timeline_pruning_and_observation_bound_keep_memory_finite() {
    let sources = source_catalog();
    let mut bounded = SourceTimeline::with_observation_limit(&sources, 3);
    for frame in 0..3_u64 {
        bounded
            .observe(
                camera(1),
                ts(i64::try_from(frame).unwrap() * FRAME),
                SequenceNumber::new(frame + 1),
                FrameIndex::new(frame),
            )
            .unwrap();
    }
    let overflow = bounded.observe(
        camera(1),
        ts(3 * FRAME),
        SequenceNumber::new(4),
        FrameIndex::new(3),
    );
    assert_eq!(
        overflow,
        Err(ReplayError::LimitExceeded {
            limit: ReplayLimit::Observations,
            maximum: 3,
        })
    );
    // Pruning to the retention cutoff frees room; the cutoff still resolves.
    assert_eq!(bounded.prune_before(ts(FRAME + 1)), 1);
    assert_eq!(bounded.observation_count(camera(1)), 2);
    assert_eq!(
        bounded
            .resolve(camera(1), ts(FRAME + 1))
            .unwrap()
            .source_frame,
        FrameIndex::new(1)
    );
    bounded
        .observe(
            camera(1),
            ts(3 * FRAME),
            SequenceNumber::new(4),
            FrameIndex::new(3),
        )
        .unwrap();

    let mut timeline = source_timeline();
    timeline
        .record_discontinuity(camera(1), ts(3 * SECOND), "signal loss")
        .unwrap();
    timeline
        .observe(
            camera(1),
            ts(3 * SECOND + FRAME),
            SequenceNumber::new(1),
            FrameIndex::new(0),
        )
        .unwrap();
    let span = timeline
        .frames_in(camera(1), range(2 * SECOND, 4 * SECOND))
        .unwrap();
    assert_eq!(span.frame_count, 51);
    assert_eq!(span.discontinuities, 1);
    let dropped = timeline.prune_before(ts(4 * SECOND));
    assert_eq!(dropped, 150 + 3 * 149);
    assert_eq!(timeline.discontinuities(), &[]);
}

#[test]
fn capture_protects_every_angle_until_the_event_is_removed() {
    let mut catalog = segment_catalog();
    let timeline = source_timeline();
    let mut database = EventDatabase::default();
    let marked = range(SECOND / 2, 3 * SECOND / 2);
    let captured = database
        .capture_event(replay_event(1, marked, &[3, 1, 2]), &mut catalog, &timeline)
        .unwrap();
    assert_eq!(captured.event_id, event_id(1));
    assert_eq!(captured.timeline, marked);
    let cameras: Vec<_> = captured
        .angles
        .iter()
        .map(|angle| angle.frames.camera_id)
        .collect();
    assert_eq!(cameras, vec![camera(1), camera(2), camera(3)]);
    for angle in &captured.angles {
        assert_eq!(angle.frames.frame_count, 50);
        assert_eq!(angle.frames.first.source_frame, FrameIndex::new(25));
        let value = angle.frames.camera_id.get().get();
        assert_eq!(
            angle.segments,
            vec![segment_id(value, 0), segment_id(value, 1)]
        );
    }

    let reference = ProtectionReference::Event(event_id(1));
    for value in 1..=3 {
        for index in 0..2 {
            assert!(
                catalog
                    .segment(segment_id(value, index))
                    .unwrap()
                    .protections
                    .contains(&reference)
            );
        }
    }
    // Camera 4 was not an angle; its oldest segment is reclaimable.
    assert_eq!(
        catalog.reclaim(root(4), 1).unwrap().deleted,
        vec![segment_id(4, 0)]
    );
    let blocked = catalog.reclaim(root(1), 3_000).unwrap();
    assert_eq!(blocked.deleted, Vec::<SegmentId>::new());
    assert_eq!(blocked.blocked_bytes, 3_000);

    database
        .add_list(EventList {
            id: ListId::new(nonzero(1)),
            name: "Goals".into(),
            events: vec![event_id(1)],
        })
        .unwrap();
    let removed = database.remove_event(event_id(1), &mut catalog).unwrap();
    assert_eq!(removed.id, event_id(1));
    assert!(database.event(event_id(1)).is_none());
    assert_eq!(
        database.list(ListId::new(nonzero(1))).unwrap().events,
        Vec::<EventId>::new()
    );
    assert_eq!(
        catalog.reclaim(root(1), 2_000).unwrap().deleted,
        vec![segment_id(1, 0), segment_id(1, 1)]
    );
    assert_eq!(
        database.remove_event(event_id(1), &mut catalog),
        Err(ReplayError::UnknownEvent(event_id(1)))
    );
}

#[test]
fn capture_is_atomic_when_any_angle_is_unrecorded() {
    let mut catalog = segment_catalog();
    let timeline = source_timeline();
    let mut database = EventDatabase::default();
    // Camera 2 lost its oldest segment to retention.
    catalog.reclaim(root(2), 1).unwrap();
    let before = catalog.clone();

    let result = database.capture_event(
        replay_event(1, range(SECOND / 2, 3 * SECOND / 2), &[1, 2]),
        &mut catalog,
        &timeline,
    );
    assert_eq!(
        result,
        Err(ReplayError::CoverageGap {
            camera: camera(2),
            gap: range(SECOND / 2, SECOND),
        })
    );
    assert!(database.event(event_id(1)).is_none());
    assert!(
        catalog
            .segments()
            .zip(before.segments())
            .all(|(after, before)| after == before)
    );

    // Covered by segments but beyond the last observed frame.
    let late = range(2 * SECOND + 990_000_000, 3 * SECOND);
    assert_eq!(
        database.capture_event(replay_event(2, late, &[1]), &mut catalog, &timeline),
        Err(ReplayError::NoFramesInRange(camera(1)))
    );
    assert!(
        catalog
            .segments()
            .all(|segment| segment.protections.is_empty())
    );
}

#[test]
fn replay_channel_angle_switch_keeps_synchronized_time() {
    let mut catalog = segment_catalog();
    let timeline = source_timeline();
    let mut database = EventDatabase::default();
    database
        .capture_event(
            replay_event(1, range(SECOND, 2 * SECOND), &[1, 2, 3]),
            &mut catalog,
            &timeline,
        )
        .unwrap();
    let live_edge = ts(2 * SECOND + 500_000_000);
    let mut decks = ReplayDecks::new(camera(1), camera(4), live_edge);

    // Live mode resolves the live edge on any camera.
    let live = decks
        .resolve_frame(ReplayChannelId::B, live_edge, &timeline, &catalog)
        .unwrap();
    assert!(live.live);
    assert_eq!(live.observation.source_frame, FrameIndex::new(124));
    assert_eq!(live.segment_id, segment_id(4, 2));
    assert_eq!(live.offset_nanos, 16_000_000);

    let event = database.event(event_id(1)).unwrap().clone();
    decks.cue_event(ReplayChannelId::A, &event);
    decks
        .seek_frame(
            ReplayChannelId::A,
            FrameIndex::new(10),
            FRAME.unsigned_abs(),
        )
        .unwrap();
    let on_one = decks
        .resolve_frame(ReplayChannelId::A, live_edge, &timeline, &catalog)
        .unwrap();
    assert!(!on_one.live);
    assert_eq!(on_one.target, ts(SECOND + 10 * FRAME));
    // Camera 1 frame 60 lands 1 ms after the target, so frame 59 is on screen.
    assert_eq!(on_one.observation.source_frame, FrameIndex::new(59));
    assert_eq!(on_one.offset_nanos, 19_000_000);

    decks
        .select_event_angle(ReplayChannelId::A, camera(3), &database)
        .unwrap();
    let on_three = decks
        .resolve_frame(ReplayChannelId::A, live_edge, &timeline, &catalog)
        .unwrap();
    assert_eq!(on_three.target, on_one.target);
    assert_eq!(on_three.camera_id, camera(3));
    assert_eq!(on_three.observation.source_frame, FrameIndex::new(59));
    assert_eq!(on_three.offset_nanos, 17_000_000);
    assert_eq!(on_three.segment_id, segment_id(3, 1));

    assert_eq!(
        decks.select_event_angle(ReplayChannelId::A, camera(4), &database),
        Err(ReplayError::AngleNotInEvent {
            event: event_id(1),
            camera: camera(4),
        })
    );
    assert_eq!(decks.channel(ReplayChannelId::A).angle, camera(3));
    // Live channels accept any configured camera.
    decks
        .select_event_angle(ReplayChannelId::B, camera(2), &database)
        .unwrap();

    // Jog advances exactly one source frame per step.
    decks
        .jog(ReplayChannelId::A, 1, FRAME.unsigned_abs())
        .unwrap();
    let jogged = decks
        .resolve_frame(ReplayChannelId::A, live_edge, &timeline, &catalog)
        .unwrap();
    assert_eq!(jogged.observation.source_frame, FrameIndex::new(60));

    // Half speed for four frame periods covers two source frames.
    decks.play(ReplayChannelId::A, PlaybackRate::from_milli(500).unwrap());
    decks.advance(ReplayChannelId::A, 4 * FRAME.unsigned_abs(), live_edge);
    let slow = decks
        .resolve_frame(ReplayChannelId::A, live_edge, &timeline, &catalog)
        .unwrap();
    assert_eq!(slow.observation.source_frame, FrameIndex::new(62));

    // Auto-return at mark-out resolves the live edge again.
    decks.set_auto_return(ReplayChannelId::A, true);
    decks.play(ReplayChannelId::A, PlaybackRate::FORWARD_1X);
    decks.advance(ReplayChannelId::A, SECOND.unsigned_abs(), live_edge);
    assert!(matches!(
        decks.channel(ReplayChannelId::A).mode,
        ChannelMode::Live
    ));
    let returned = decks
        .resolve_frame(ReplayChannelId::A, live_edge, &timeline, &catalog)
        .unwrap();
    assert!(returned.live);
    assert_eq!(returned.observation.source_frame, FrameIndex::new(124));

    // A cued event deleted underneath the channel cannot be angle-switched.
    decks.cue_event(ReplayChannelId::A, &event);
    database.remove_event(event_id(1), &mut catalog).unwrap();
    assert_eq!(
        decks.select_event_angle(ReplayChannelId::A, camera(2), &database),
        Err(ReplayError::UnknownEvent(event_id(1)))
    );
}

#[test]
fn channel_resolution_reports_missing_media() {
    let mut catalog = segment_catalog();
    let timeline = source_timeline();
    let mut database = EventDatabase::default();
    database
        .insert_event(replay_event(1, range(0, SECOND), &[1]))
        .unwrap();
    let mut decks = ReplayDecks::new(camera(1), camera(2), ts(0));
    decks.cue_event(ReplayChannelId::A, database.event(event_id(1)).unwrap());
    // Uncaptured (unprotected) media can be reclaimed out from under a cue.
    catalog.reclaim(root(1), 1).unwrap();
    assert_eq!(
        decks.resolve_frame(ReplayChannelId::A, ts(0), &timeline, &catalog),
        Err(ReplayError::ObservationUnavailable(camera(1)))
    );
    decks
        .jog(ReplayChannelId::A, 5, FRAME.unsigned_abs())
        .unwrap();
    assert_eq!(
        decks.resolve_frame(ReplayChannelId::A, ts(0), &timeline, &catalog),
        Err(ReplayError::SegmentUnavailable {
            camera: camera(1),
            at: ts(4 * FRAME + skew(1)),
        })
    );
}

#[test]
fn event_database_enforces_explicit_bounds() {
    let mut database = EventDatabase::default();
    database
        .insert_event(replay_event(1, range(0, SECOND), &[1, 2]))
        .unwrap();

    for value in 1..=MAX_LISTS {
        database
            .add_list(EventList {
                id: ListId::new(nonzero(u128::try_from(value).unwrap())),
                name: format!("List {value}"),
                events: Vec::new(),
            })
            .unwrap();
    }
    const { assert!(MAX_LISTS >= 20) };
    assert_eq!(database.lists().count(), MAX_LISTS);
    assert_eq!(
        database.add_list(EventList {
            id: ListId::new(nonzero(1_000)),
            name: "One too many".into(),
            events: Vec::new(),
        }),
        Err(ReplayError::LimitExceeded {
            limit: ReplayLimit::Lists,
            maximum: MAX_LISTS,
        })
    );

    for value in 0..MAX_TAGS_PER_EVENT {
        database
            .add_tag(event_id(1), format!("tag-{value:02}"))
            .unwrap();
    }
    // Re-adding an existing tag is idempotent even at the bound.
    database.add_tag(event_id(1), "tag-00").unwrap();
    assert_eq!(
        database.add_tag(event_id(1), "overflow"),
        Err(ReplayError::LimitExceeded {
            limit: ReplayLimit::TagsPerEvent,
            maximum: MAX_TAGS_PER_EVENT,
        })
    );
    assert_eq!(database.events_with_tag("tag-07"), vec![event_id(1)]);
    assert_eq!(database.events_with_tag("missing"), Vec::<EventId>::new());

    assert_eq!(
        database.set_note(event_id(1), "n".repeat(MAX_NOTE_BYTES + 1)),
        Err(ReplayError::LimitExceeded {
            limit: ReplayLimit::NoteBytes,
            maximum: MAX_NOTE_BYTES,
        })
    );
    database
        .set_note(event_id(1), "n".repeat(MAX_NOTE_BYTES))
        .unwrap();

    let mut long_name = replay_event(2, range(0, SECOND), &[1]);
    long_name.name = "x".repeat(MAX_NAME_BYTES + 1);
    assert_eq!(
        database.insert_event(long_name),
        Err(ReplayError::LimitExceeded {
            limit: ReplayLimit::NameBytes,
            maximum: MAX_NAME_BYTES,
        })
    );

    let folder = FolderId::new(nonzero(7));
    let mut foldered = replay_event(3, range(0, SECOND), &[1]);
    foldered.folders.insert(folder);
    assert_eq!(
        database.insert_event(foldered.clone()),
        Err(ReplayError::UnknownFolder(folder))
    );
    database
        .add_folder(EventFolder {
            id: folder,
            name: "Second half".into(),
        })
        .unwrap();
    database.insert_event(foldered).unwrap();
    assert_eq!(database.events_in_folder(folder), vec![event_id(3)]);
}

#[test]
fn list_rejects_repeated_events_on_creation() {
    let mut database = EventDatabase::default();
    database
        .insert_event(replay_event(1, range(0, SECOND), &[1]))
        .unwrap();
    assert_eq!(
        database.add_list(EventList {
            id: ListId::new(nonzero(1)),
            name: "Repeated".into(),
            events: vec![event_id(1), event_id(1)],
        }),
        Err(ReplayError::EventAlreadyInList {
            event: event_id(1),
            list: ListId::new(nonzero(1)),
        })
    );
    assert!(database.list(ListId::new(nonzero(1))).is_none());
}
