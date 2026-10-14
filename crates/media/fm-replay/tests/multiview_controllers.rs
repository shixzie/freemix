//! Synchronized Quad View / Replay multi-view tile planning, the dual
//! controller action surface, and bitrate-driven disk health prediction.

use std::{collections::BTreeSet, num::NonZeroU128, path::PathBuf};

use fm_frame::{ClockDomainId, NormalizedTimestamp, SequenceNumber};
use fm_playback::FrameIndex;
use fm_record::RecorderId;
use fm_replay::{
    AngleSlot, CameraBitrate, CameraId, CameraSource, ChannelMode, ChannelTransport, ControlInput,
    ControllerContext, ControllerError, ControllerProfile, ControllerSlot, ControllerSurface,
    DiskHealthError, DiskHealthLevel, DiskHealthMonitor, DiskSample, DiskThresholds, EventDatabase,
    EventId, JogStep, MAX_BINDINGS_PER_PROFILE, Modifiers, MultiviewError, MultiviewLayout,
    PlaybackRate, RecordingLoad, ReplayAction, ReplayChannelId, ReplayDecks, ReplayError,
    ReplayEvent, ReplayMarks, RollingSegmentCatalog, SegmentId, SegmentMetadata, SegmentState,
    SourceCatalog, SourceTimeline, StorageRoot, StorageRootId, TileMode, TileSource, TimelineRange,
    binding_conflicts,
};
use fm_types::{Channel, ChannelLayout};

const SECOND: i64 = 1_000_000_000;
/// 50 fps frame cadence.
const FRAME: i64 = 20_000_000;
const MS: i64 = 1_000_000;
const TOL_MS: u64 = 1_000_000;
const CAMERAS: u128 = 8;

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn camera(value: u128) -> CameraId {
    CameraId::new(nonzero(value))
}

/// Cameras 1–4 record to root 1, cameras 5–8 to root 2.
fn root_of(value: u128) -> StorageRootId {
    StorageRootId::new(nonzero(if value <= 4 { 1 } else { 2 }))
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

fn skew(camera_value: u128) -> i64 {
    i64::try_from(camera_value).unwrap() * MS
}

fn source_catalog() -> SourceCatalog {
    SourceCatalog::new(
        (1..=CAMERAS)
            .map(|value| CameraSource {
                id: camera(value),
                name: format!("Camera {value}"),
                storage_root_id: root_of(value),
                recorder_id: RecorderId::new(nonzero(value)),
                clock_domain: ClockDomainId::new(nonzero(value)),
                audio_layout: ChannelLayout::new(vec![Channel::Left, Channel::Right]).unwrap(),
            })
            .collect(),
    )
    .unwrap()
}

/// Every camera records one-second segments covering 0..3 s.
fn segment_catalog() -> RollingSegmentCatalog {
    let mut catalog = RollingSegmentCatalog::default();
    for root in [1, 2] {
        catalog
            .add_root(StorageRoot {
                id: StorageRootId::new(nonzero(root)),
                path: PathBuf::from(format!("/replay/root-{root}")),
                capacity_bytes: 1_000_000,
            })
            .unwrap();
    }
    for value in 1..=CAMERAS {
        catalog
            .assign_camera(camera(value), root_of(value))
            .unwrap();
        for index in 0..3_u64 {
            let start = i64::try_from(index).unwrap() * SECOND;
            catalog
                .insert_segment(SegmentMetadata {
                    id: segment_id(value, index),
                    camera_id: camera(value),
                    storage_root_id: root_of(value),
                    recorder_segment_index: index,
                    timeline: range(start, start + SECOND),
                    path: PathBuf::from(format!("/replay/{value}/{index}.seg")),
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

/// 150 frames per camera at `n * 20 ms + camera ms`.
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
        id: EventId::new(nonzero(id)),
        name: format!("Event {id}"),
        timeline,
        angles: angles.iter().copied().map(camera).collect(),
        preferred_angle: camera(angles[0]),
        tags: BTreeSet::new(),
        note: String::new(),
        folders: BTreeSet::new(),
    }
}

fn quad() -> MultiviewLayout {
    MultiviewLayout::quad([camera(1), camera(2), camera(3), camera(4)]).unwrap()
}

#[test]
fn live_quad_view_resolves_each_angle_at_the_live_edge() {
    let timeline = source_timeline();
    let catalog = segment_catalog();
    let decks = ReplayDecks::new(camera(1), camera(2), ts(0));
    let live_edge = ts(100 * FRAME + 10 * MS);

    let plan = quad()
        .plan(
            &decks,
            ReplayChannelId::A,
            live_edge,
            &timeline,
            &catalog,
            3 * TOL_MS,
        )
        .unwrap();

    assert_eq!((plan.rows, plan.columns), (2, 2));
    assert_eq!(plan.target, live_edge);
    assert_eq!(plan.driver_mode, TileMode::Live);
    assert_eq!(plan.driver_mode.label(), "LIVE");
    let placed: Vec<_> = plan
        .tiles
        .iter()
        .map(|tile| (tile.index, tile.row, tile.column, tile.source))
        .collect();
    assert_eq!(
        placed,
        vec![
            (0, 0, 0, TileSource::Angle(camera(1))),
            (1, 0, 1, TileSource::Angle(camera(2))),
            (2, 1, 0, TileSource::Angle(camera(3))),
            (3, 1, 1, TileSource::Angle(camera(4))),
        ]
    );
    for (value, tile) in (1..=4).zip(&plan.tiles) {
        assert_eq!(tile.mode, TileMode::Live);
        assert!(tile.frame.live);
        assert_eq!(tile.frame.camera_id, camera(value));
        assert_eq!(tile.frame.observation.source_frame, FrameIndex::new(100));
        assert_eq!(tile.frame.segment_id, segment_id(value, 2));
        assert_eq!(
            tile.frame.offset_nanos,
            u64::try_from(10 * MS - skew(value)).unwrap()
        );
    }
    assert_eq!(plan.skew_nanos, 3_000_000);
    assert!(plan.in_sync);

    let strict = quad()
        .plan(
            &decks,
            ReplayChannelId::A,
            live_edge,
            &timeline,
            &catalog,
            TOL_MS,
        )
        .unwrap();
    assert!(!strict.in_sync);
    assert_eq!(strict.tiles, plan.tiles);
}

#[test]
fn recorded_quad_view_follows_the_channel_cursor_and_matches_source_resolution() {
    let timeline = source_timeline();
    let catalog = segment_catalog();
    let event = replay_event(7, range(SECOND, 2 * SECOND), &[2, 1, 3, 4]);
    let mut decks = ReplayDecks::new(camera(1), camera(2), ts(0));
    decks.cue_event(ReplayChannelId::B, &event);
    decks.jog(ReplayChannelId::B, 5, 20_000_000).unwrap();
    let cursor = ts(SECOND + 5 * FRAME);
    assert_eq!(decks.channel(ReplayChannelId::B).cursor, cursor);
    let before = decks.clone();

    let plan = quad()
        .plan(
            &decks,
            ReplayChannelId::B,
            ts(150 * FRAME),
            &timeline,
            &catalog,
            5 * TOL_MS,
        )
        .unwrap();

    assert_eq!(decks, before, "planning never mutates the decks");
    assert_eq!(plan.target, cursor);
    assert_eq!(plan.driver_mode, TileMode::Recorded { event_id: event.id });
    assert_eq!(plan.driver_mode.label(), "REPLAY");
    for (value, tile) in (1..=4).zip(plan.angle_tiles()) {
        let expected = timeline.resolve(camera(value), cursor).unwrap();
        assert_eq!(tile.frame.observation, expected);
        // Frame 55 lands at 1.1 s + camera skew, after the cursor.
        assert_eq!(tile.frame.observation.source_frame, FrameIndex::new(54));
        assert_eq!(tile.frame.segment_id, segment_id(value, 1));
        assert!(!tile.frame.live);
        assert_eq!(tile.frame.channel, ReplayChannelId::B);
    }
    assert!(plan.in_sync);
}

#[test]
fn eight_angle_multiview_places_channel_tiles_first_with_their_own_modes() {
    let timeline = source_timeline();
    let catalog = segment_catalog();
    let event = replay_event(3, range(SECOND, 2 * SECOND), &[5, 6]);
    let mut decks = ReplayDecks::new(camera(1), camera(8), ts(0));
    decks.cue_event(ReplayChannelId::A, &event);
    let live_edge = ts(140 * FRAME + 9 * MS);

    let layout = MultiviewLayout::new(
        vec![ReplayChannelId::A, ReplayChannelId::B],
        (1..=CAMERAS).rev().map(camera).collect(),
    )
    .unwrap();
    assert_eq!(layout.tile_count(), 10);
    let plan = layout
        .plan(
            &decks,
            ReplayChannelId::B,
            live_edge,
            &timeline,
            &catalog,
            8 * TOL_MS,
        )
        .unwrap();

    assert_eq!((plan.rows, plan.columns), (3, 4));
    assert_eq!(plan.tiles.len(), 10);
    let a = plan.tiles[0];
    assert_eq!(a.source, TileSource::Channel(ReplayChannelId::A));
    assert_eq!(a.mode, TileMode::Recorded { event_id: event.id });
    assert_eq!(a.frame.camera_id, camera(5));
    assert_eq!(a.frame.target, ts(SECOND));
    let b = plan.tiles[1];
    assert_eq!(b.source, TileSource::Channel(ReplayChannelId::B));
    assert_eq!(b.mode, TileMode::Live);
    assert_eq!(b.frame.camera_id, camera(8));
    assert_eq!(b.frame.observation.source_frame, FrameIndex::new(140));

    let angles: Vec<_> = plan.angle_tiles().map(|tile| tile.source).collect();
    let expected: Vec<_> = (1..=CAMERAS)
        .rev()
        .map(|value| TileSource::Angle(camera(value)))
        .collect();
    assert_eq!(angles, expected);
    assert!(plan.angle_tiles().all(|tile| tile.mode == TileMode::Live));
    assert_eq!(plan.tiles[9].row, 2);
    assert_eq!(plan.tiles[9].column, 1);
    assert_eq!(plan.skew_nanos, 7_000_000);
    assert!(plan.in_sync);
}

#[test]
fn multiview_layouts_and_unresolved_tiles_fail_explicitly() {
    assert_eq!(
        MultiviewLayout::new(vec![], vec![]),
        Err(MultiviewError::NoAngles)
    );
    assert_eq!(
        MultiviewLayout::new(vec![], (1..=9).map(camera).collect()),
        Err(MultiviewError::TooManyAngles(9))
    );
    assert_eq!(
        MultiviewLayout::quad([camera(1), camera(2), camera(1), camera(4)]),
        Err(MultiviewError::DuplicateAngle(camera(1)))
    );
    assert_eq!(
        MultiviewLayout::new(
            vec![ReplayChannelId::B, ReplayChannelId::B],
            vec![camera(1)]
        ),
        Err(MultiviewError::DuplicateChannel(ReplayChannelId::B))
    );

    let timeline = source_timeline();
    let catalog = segment_catalog();
    let decks = ReplayDecks::new(camera(1), camera(2), ts(0));
    let layout = MultiviewLayout::new(vec![], vec![camera(1), camera(9)]).unwrap();
    assert_eq!(
        layout.plan(
            &decks,
            ReplayChannelId::A,
            ts(SECOND),
            &timeline,
            &catalog,
            0
        ),
        Err(MultiviewError::Unresolved {
            source: TileSource::Angle(camera(9)),
            error: ReplayError::UnknownCamera(camera(9)),
        })
    );
    // Before camera 1's first frame (1 ms) nothing can be presented.
    assert_eq!(
        quad().plan(&decks, ReplayChannelId::A, ts(0), &timeline, &catalog, 0),
        Err(MultiviewError::Unresolved {
            source: TileSource::Angle(camera(1)),
            error: ReplayError::ObservationUnavailable(camera(1)),
        })
    );
}

fn key(code: u16) -> ControlInput {
    ControlInput::Key {
        code,
        modifiers: Modifiers::default(),
    }
}

fn shift_b() -> ControlInput {
    ControlInput::Key {
        code: 66,
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    }
}

fn slot(value: u8) -> AngleSlot {
    AngleSlot::new(value).unwrap()
}

fn primary_profile() -> ControllerProfile {
    ControllerProfile::new(
        "Operator A keyboard",
        ReplayChannelId::A,
        vec![
            (key(73), ReplayAction::MarkIn),
            (key(79), ReplayAction::MarkOut),
            (
                key(32),
                ReplayAction::Play(PlaybackRate::from_milli(500).unwrap()),
            ),
            (key(80), ReplayAction::Pause),
            (key(39), ReplayAction::Jog(JogStep::new(1).unwrap())),
            (key(37), ReplayAction::Jog(JogStep::new(-1).unwrap())),
            (key(49), ReplayAction::SelectAngle(slot(1))),
            (key(50), ReplayAction::SelectAngle(slot(2))),
            (key(51), ReplayAction::SelectAngle(slot(3))),
            (key(76), ReplayAction::ToggleLive),
            (shift_b(), ReplayAction::SelectChannel(ReplayChannelId::B)),
        ],
    )
    .unwrap()
}

fn secondary_profile(extra: Vec<(ControlInput, ReplayAction)>) -> ControllerProfile {
    let mut bindings = vec![
        // Same button and note numbers as any other device never conflict.
        (ControlInput::Button(1), ReplayAction::MarkIn),
        (
            ControlInput::Button(2),
            ReplayAction::Shuttle(PlaybackRate::from_milli(-2_000).unwrap()),
        ),
        (
            ControlInput::MidiNote {
                channel: 0,
                note: 60,
            },
            ReplayAction::ToggleLive,
        ),
        (
            ControlInput::Button(3),
            ReplayAction::SelectChannel(ReplayChannelId::A),
        ),
    ];
    bindings.extend(extra);
    ControllerProfile::new("Operator B controller", ReplayChannelId::B, bindings).unwrap()
}

#[test]
fn dual_controller_profiles_reject_shared_key_conflicts() {
    let primary = primary_profile();
    let clash = secondary_profile(vec![
        (key(32), ReplayAction::Pause),
        (key(79), ReplayAction::MarkOut),
        (
            ControlInput::Key {
                code: 32,
                modifiers: Modifiers {
                    control: true,
                    ..Modifiers::default()
                },
            },
            ReplayAction::Pause,
        ),
    ]);
    let conflicts = binding_conflicts(&primary, &clash);
    assert_eq!(
        conflicts
            .iter()
            .map(|conflict| conflict.input)
            .collect::<Vec<_>>(),
        vec![key(32), key(79)]
    );
    assert_eq!(conflicts[0].secondary, ReplayAction::Pause);
    assert_eq!(
        ControllerSurface::new(primary.clone(), clash),
        Err(ControllerError::SharedInputConflict(conflicts[0]))
    );

    let mirrored = ControllerProfile::new(
        "Mirror",
        ReplayChannelId::B,
        vec![(ControlInput::Button(1), ReplayAction::Pause)],
    )
    .unwrap();
    let with_buttons = ControllerProfile::new(
        "Buttons",
        ReplayChannelId::A,
        vec![(ControlInput::Button(1), ReplayAction::MarkIn)],
    )
    .unwrap();
    assert_eq!(binding_conflicts(&with_buttons, &mirrored), Vec::new());
    assert!(ControllerSurface::new(primary, secondary_profile(vec![])).is_ok());
}

#[test]
fn controller_profiles_and_actions_are_bounded() {
    assert_eq!(JogStep::new(0), Err(ControllerError::InvalidJogStep(0)));
    assert_eq!(JogStep::new(101), Err(ControllerError::InvalidJogStep(101)));
    assert_eq!(JogStep::new(-100).unwrap().frames(), -100);
    assert_eq!(AngleSlot::new(0), Err(ControllerError::InvalidAngleSlot(0)));
    assert_eq!(AngleSlot::new(9), Err(ControllerError::InvalidAngleSlot(9)));
    assert_eq!(
        ControllerProfile::new(" ", ReplayChannelId::A, vec![]),
        Err(ControllerError::Replay(ReplayError::EmptyName))
    );
    assert_eq!(
        ControllerProfile::new(
            "x",
            ReplayChannelId::A,
            vec![
                (key(1), ReplayAction::Pause),
                (key(1), ReplayAction::MarkIn)
            ]
        ),
        Err(ControllerError::DuplicateBinding(key(1)))
    );
    let midi = ControlInput::MidiNote {
        channel: 16,
        note: 1,
    };
    assert_eq!(
        ControllerProfile::new("x", ReplayChannelId::A, vec![(midi, ReplayAction::Pause)]),
        Err(ControllerError::InvalidInput(midi))
    );
    let too_many = (0..=u16::try_from(MAX_BINDINGS_PER_PROFILE).unwrap())
        .map(|code| (ControlInput::Button(code), ReplayAction::Pause))
        .collect();
    assert_eq!(
        ControllerProfile::new("x", ReplayChannelId::A, too_many),
        Err(ControllerError::TooManyBindings(
            MAX_BINDINGS_PER_PROFILE + 1
        ))
    );
}

#[test]
fn controller_commands_drive_independent_channels() {
    let mut events = EventDatabase::default();
    let event = replay_event(11, range(SECOND, 2 * SECOND), &[1, 2]);
    events.insert_event(event.clone()).unwrap();
    let angles = [camera(1), camera(2), camera(3)];
    let live_edge = ts(2 * SECOND);
    let context = ControllerContext {
        live_edge,
        frame_duration_nanos: 20_000_000,
        angles: &angles,
        events: &events,
        recall: Some(event.id),
    };
    let mut surface = ControllerSurface::new(primary_profile(), secondary_profile(vec![])).unwrap();
    let mut decks = ReplayDecks::new(camera(1), camera(2), ts(0));
    let mut marks = ReplayMarks::default();
    macro_rules! press {
        ($surface:expr, $decks:expr, $slot:expr, $input:expr $(,)?) => {
            $surface
                .dispatch($slot, $input)
                .expect("bound input")
                .apply($decks, &mut marks, &context)
        };
    }

    assert_eq!(surface.dispatch(ControllerSlot::Primary, key(1)), None);
    assert_eq!(
        surface.dispatch(ControllerSlot::Secondary, key(73)),
        None,
        "keys bound only by the primary profile do nothing on the secondary"
    );

    // Primary marks in on live channel A at the live edge.
    press!(&mut surface, &mut decks, ControllerSlot::Primary, key(73)).unwrap();
    // Live → recall the event on A, jog forward, play at 50 %.
    press!(&mut surface, &mut decks, ControllerSlot::Primary, key(76)).unwrap();
    press!(&mut surface, &mut decks, ControllerSlot::Primary, key(39)).unwrap();
    press!(&mut surface, &mut decks, ControllerSlot::Primary, key(32)).unwrap();
    let a = decks.channel(ReplayChannelId::A);
    assert!(matches!(a.mode, ChannelMode::Recorded { event_id, .. } if event_id == event.id));
    assert_eq!(a.cursor, ts(SECOND + FRAME));
    assert_eq!(a.rate.as_milli(), 500);
    assert_eq!(a.transport, ChannelTransport::Playing);
    // Mark out at the recorded cursor.
    press!(&mut surface, &mut decks, ControllerSlot::Primary, key(79)).unwrap();
    assert_eq!(marks.mark_in, Some(live_edge));
    assert_eq!(marks.mark_out, Some(ts(SECOND + FRAME)));

    // Angle slot 2 selects camera 2 (an event angle); slot 3 is not in the event.
    press!(&mut surface, &mut decks, ControllerSlot::Primary, key(50)).unwrap();
    assert_eq!(decks.channel(ReplayChannelId::A).angle, camera(2));
    assert_eq!(
        press!(&mut surface, &mut decks, ControllerSlot::Primary, key(51)),
        Err(ControllerError::Replay(ReplayError::AngleNotInEvent {
            event: event.id,
            camera: camera(3),
        }))
    );

    // Secondary shuttles its own channel B in reverse without touching A.
    let a_before = decks.channel(ReplayChannelId::A).clone();
    press!(
        &mut surface,
        &mut decks,
        ControllerSlot::Secondary,
        ControlInput::Button(2),
    )
    .unwrap();
    assert_eq!(decks.channel(ReplayChannelId::A), &a_before);
    assert_eq!(
        decks.channel(ReplayChannelId::B).transport,
        ChannelTransport::Shuttle
    );
    assert_eq!(decks.channel(ReplayChannelId::B).rate.as_milli(), -2_000);

    // Shift+B retargets only the primary controller to channel B.
    let command = surface
        .dispatch(ControllerSlot::Primary, shift_b())
        .unwrap();
    assert_eq!(command.channel, ReplayChannelId::B);
    assert_eq!(
        surface.target_channel(ControllerSlot::Primary),
        ReplayChannelId::B
    );
    assert_eq!(
        surface.target_channel(ControllerSlot::Secondary),
        ReplayChannelId::B
    );
    press!(&mut surface, &mut decks, ControllerSlot::Primary, key(80)).unwrap();
    assert_eq!(
        decks.channel(ReplayChannelId::B).transport,
        ChannelTransport::Paused
    );

    // Toggle live on recorded channel A from the secondary after retargeting.
    press!(
        &mut surface,
        &mut decks,
        ControllerSlot::Secondary,
        ControlInput::Button(3),
    )
    .unwrap();
    press!(
        &mut surface,
        &mut decks,
        ControllerSlot::Secondary,
        ControlInput::MidiNote {
            channel: 0,
            note: 60,
        },
    )
    .unwrap();
    assert_eq!(decks.channel(ReplayChannelId::A).mode, ChannelMode::Live);
    assert_eq!(decks.channel(ReplayChannelId::A).cursor, live_edge);
}

#[test]
fn controller_apply_errors_leave_decks_unchanged() {
    let events = EventDatabase::default();
    let angles = [camera(1)];
    let context = ControllerContext {
        live_edge: ts(SECOND),
        frame_duration_nanos: 0,
        angles: &angles,
        events: &events,
        recall: None,
    };
    let mut surface = ControllerSurface::new(primary_profile(), secondary_profile(vec![])).unwrap();
    let mut decks = ReplayDecks::new(camera(1), camera(2), ts(0));
    let before = decks.clone();
    let mut marks = ReplayMarks::default();
    for (input, expected) in [
        (key(76), ControllerError::NothingToRecall),
        (key(50), ControllerError::UnassignedAngle(slot(2))),
        (
            key(39),
            ControllerError::Replay(ReplayError::FrameDurationZero),
        ),
    ] {
        let command = surface.dispatch(ControllerSlot::Primary, input).unwrap();
        assert_eq!(
            command.apply(&mut decks, &mut marks, &context),
            Err(expected)
        );
        assert_eq!(decks, before);
    }
    let missing = ControllerContext {
        recall: Some(EventId::new(nonzero(99))),
        ..context
    };
    let command = surface.dispatch(ControllerSlot::Primary, key(76)).unwrap();
    assert_eq!(
        command.apply(&mut decks, &mut marks, &missing),
        Err(ControllerError::Replay(ReplayError::UnknownEvent(
            EventId::new(nonzero(99))
        )))
    );
    assert_eq!(decks, before);
    assert_eq!(marks, ReplayMarks::default());
}

fn bitrate(value: u128, video: u64, audio: u64) -> CameraBitrate {
    CameraBitrate {
        camera_id: camera(value),
        video_bits_per_second: video,
        audio_bits_per_second: audio,
    }
}

#[test]
fn recording_load_converts_bitrates_per_camera_and_root() {
    let load = RecordingLoad::new(
        vec![
            bitrate(1, 50_000_000, 256_000),
            bitrate(2, 50_000_000, 256_000),
            bitrate(5, 8_000_001, 0),
        ],
        20,
    )
    .unwrap();
    // (50_256_000 * 1.02) / 8 = 6_407_640 bytes/s.
    assert_eq!(load.camera_bytes_per_second(camera(1)), Some(6_407_640));
    // 8_000_001 * 1.02 / 8 = 1_020_000.1275 → rounded up.
    assert_eq!(load.camera_bytes_per_second(camera(5)), Some(1_020_001));
    assert_eq!(load.camera_bytes_per_second(camera(3)), None);
    assert_eq!(load.total_bytes_per_second(), 2 * 6_407_640 + 1_020_001);
    let sources = source_catalog();
    assert_eq!(
        load.root_bytes_per_second(&sources, root_of(1)),
        Ok(2 * 6_407_640)
    );
    assert_eq!(
        load.root_bytes_per_second(&sources, root_of(5)),
        Ok(1_020_001)
    );

    let stray = RecordingLoad::new(vec![bitrate(9, 1, 0)], 0).unwrap();
    assert_eq!(
        stray.root_bytes_per_second(&sources, root_of(1)),
        Err(DiskHealthError::UnknownCamera(camera(9)))
    );
    assert_eq!(
        RecordingLoad::new((1..=9).map(|v| bitrate(v, 1, 0)).collect(), 0),
        Err(DiskHealthError::TooManyCameras(9))
    );
    assert_eq!(
        RecordingLoad::new(vec![bitrate(1, 1, 0), bitrate(1, 2, 0)], 0),
        Err(DiskHealthError::DuplicateCamera(camera(1)))
    );
    assert_eq!(
        RecordingLoad::new(vec![bitrate(1, 0, 0)], 0),
        Err(DiskHealthError::InvalidBitrate(camera(1)))
    );
    assert_eq!(
        RecordingLoad::new(vec![bitrate(1, 4_000_000_000, 1)], 0),
        Err(DiskHealthError::InvalidBitrate(camera(1)))
    );
    assert_eq!(
        RecordingLoad::new(vec![bitrate(1, u64::MAX, 1)], 0),
        Err(DiskHealthError::InvalidBitrate(camera(1)))
    );
    assert_eq!(
        RecordingLoad::new(vec![], 1_001),
        Err(DiskHealthError::InvalidOverhead(1_001))
    );
}

#[test]
fn disk_health_escalates_immediately_and_clears_with_hysteresis() {
    assert_eq!(
        DiskThresholds::new(600, 600, 60, 0),
        Err(DiskHealthError::InvalidThresholds)
    );
    assert_eq!(
        DiskThresholds::new(600, 0, 60, 0),
        Err(DiskHealthError::InvalidThresholds)
    );
    assert_eq!(
        DiskThresholds::new(u64::MAX, 1, 1, 0),
        Err(DiskHealthError::InvalidThresholds)
    );
    // Warn below 600 s, critical below 120 s, clear 60 s past, 1 MB reserve.
    let thresholds = DiskThresholds::new(600, 120, 60, 1_000_000).unwrap();
    let mut monitor = DiskHealthMonitor::new(thresholds);
    let rate = 1_000_000;
    let sample = |seconds: u64| DiskSample {
        free_bytes: 1_000_000 + seconds * rate,
        sustained_write_bytes_per_second: None,
    };
    let mut step = |seconds| monitor.observe(rate, sample(seconds));

    let report = step(1_000);
    assert_eq!(report.remaining_seconds, Some(1_000));
    assert_eq!(report.usable_bytes, 1_000_000_000);
    assert_eq!(report.level, DiskHealthLevel::Healthy);
    assert!(!report.changed());

    let levels: Vec<_> = [599, 640, 660, 119, 150, 179, 180, 599, 660, 0, 30, 1_000]
        .into_iter()
        .map(|seconds| step(seconds).level)
        .collect();
    assert_eq!(
        levels,
        vec![
            DiskHealthLevel::Warning,
            DiskHealthLevel::Warning, // inside the 600..660 clearing band
            DiskHealthLevel::Healthy,
            DiskHealthLevel::Critical,
            DiskHealthLevel::Critical,
            DiskHealthLevel::Critical,
            DiskHealthLevel::Warning, // 180 clears critical but not warning
            DiskHealthLevel::Warning,
            DiskHealthLevel::Healthy,
            DiskHealthLevel::Full,
            DiskHealthLevel::Critical,
            DiskHealthLevel::Healthy,
        ]
    );

    // Free space inside the reserve is full; an idle load predicts nothing.
    let mut monitor = DiskHealthMonitor::new(thresholds);
    let full = monitor.observe(
        rate,
        DiskSample {
            free_bytes: 999_999,
            sustained_write_bytes_per_second: Some(400_000),
        },
    );
    assert_eq!(full.usable_bytes, 0);
    assert_eq!(full.level, DiskHealthLevel::Full);
    assert_eq!(full.previous, DiskHealthLevel::Healthy);
    assert!(full.changed());
    assert_eq!(full.throughput_shortfall, Some(600_000));
    let idle = monitor.observe(
        0,
        DiskSample {
            free_bytes: 5_000_000,
            sustained_write_bytes_per_second: Some(400_000),
        },
    );
    assert_eq!(idle.remaining_seconds, None);
    assert_eq!(idle.level, DiskHealthLevel::Healthy);
    assert_eq!(idle.throughput_shortfall, Some(0));
    assert_eq!(monitor.level(), DiskHealthLevel::Healthy);
}
