//! Highlight reel compilation, background music bed planning, and
//! export-during-record job planning.

use std::{collections::BTreeSet, num::NonZeroU128, path::PathBuf};

use fm_frame::{ClockDomainId, NormalizedTimestamp, SequenceNumber};
use fm_playback::FrameIndex;
use fm_record::RecorderId;
use fm_replay::{
    BedGain, BedRepeat, CameraId, CameraSource, DuckingSpec, EventDatabase, EventId, ExportJobId,
    HighlightId, HighlightReel, MAX_BED_ENTRIES, MAX_EXPORT_CHUNK_FRAMES, MixWeight, MusicBedError,
    MusicBedSpec, MusicTrack, MusicTrackId, ProtectionReference, RecordExportError,
    RecordExportFailure, RecordExportQueue, RecordExportState, ReelClipSpec, ReelError,
    ReelFrameRate, ReelSpeed, ReelTransition, ReplayError, ReplayEvent, RollingSegmentCatalog,
    SegmentId, SegmentMetadata, SegmentState, SourceCatalog, SourceTimeline, StorageRoot,
    StorageRootId, TimelineRange,
};
use fm_types::{Channel, ChannelLayout};

const SECOND: i64 = 1_000_000_000;
const MS: i64 = 1_000_000;
/// 50 fps source cadence.
const FRAME: i64 = 20 * MS;
const UNITY: u32 = 65_536;

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn camera(value: u128) -> CameraId {
    CameraId::new(nonzero(value))
}

fn root() -> StorageRootId {
    StorageRootId::new(nonzero(1))
}

fn segment_id(camera_value: u128, index: u64) -> SegmentId {
    SegmentId::new(nonzero(camera_value * 100 + u128::from(index)))
}

fn clip_id(value: u128) -> HighlightId {
    HighlightId::new(nonzero(value))
}

fn job_id(value: u128) -> ExportJobId {
    ExportJobId::new(nonzero(value))
}

fn track(value: u128, duration_samples: u64) -> MusicTrack {
    MusicTrack {
        id: MusicTrackId::new(nonzero(value)),
        duration_samples,
    }
}

fn gain(q16: u32) -> BedGain {
    BedGain::from_q16(q16).unwrap()
}

fn ts(nanos: i64) -> NormalizedTimestamp {
    NormalizedTimestamp::from_nanos(nanos)
}

fn range(start: i64, end: i64) -> TimelineRange {
    TimelineRange::new(ts(start), ts(end)).unwrap()
}

fn source_catalog() -> SourceCatalog {
    SourceCatalog::new(
        (1..=2)
            .map(|value| CameraSource {
                id: camera(value),
                name: format!("Camera {value}"),
                storage_root_id: root(),
                recorder_id: RecorderId::new(nonzero(value)),
                clock_domain: ClockDomainId::new(nonzero(value)),
                audio_layout: ChannelLayout::new(vec![Channel::Left, Channel::Right]).unwrap(),
            })
            .collect(),
    )
    .unwrap()
}

/// Cameras 1 and 2 record one-second segments over 0..3 s; the last is still
/// recording (open).
fn segment_catalog() -> RollingSegmentCatalog {
    let mut catalog = RollingSegmentCatalog::default();
    catalog
        .add_root(StorageRoot {
            id: root(),
            path: PathBuf::from("/replay/root"),
            capacity_bytes: 1_000_000,
        })
        .unwrap();
    for value in 1..=2 {
        catalog.assign_camera(camera(value), root()).unwrap();
        for index in 0..3_u64 {
            let start = i64::try_from(index).unwrap() * SECOND;
            catalog
                .insert_segment(SegmentMetadata {
                    id: segment_id(value, index),
                    camera_id: camera(value),
                    storage_root_id: root(),
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
        for value in 1..=2_u128 {
            timeline
                .observe(
                    camera(value),
                    ts(i64::try_from(frame).unwrap() * FRAME + i64::try_from(value).unwrap() * MS),
                    SequenceNumber::new(frame + 1),
                    FrameIndex::new(frame),
                )
                .unwrap();
        }
    }
    timeline
}

fn events() -> EventDatabase {
    let mut database = EventDatabase::default();
    for (id, timeline) in [
        (1, range(0, SECOND)),
        (2, range(SECOND, 2 * SECOND)),
        (3, range(2 * SECOND, 2 * SECOND + 500 * MS)),
        (4, range(2 * SECOND + 500 * MS, 3 * SECOND + 500 * MS)),
    ] {
        database
            .insert_event(ReplayEvent {
                id: EventId::new(nonzero(id)),
                name: format!("Event {id}"),
                timeline,
                angles: [camera(1), camera(2)].into_iter().collect(),
                preferred_angle: camera(1),
                tags: BTreeSet::new(),
                note: String::new(),
                folders: BTreeSet::new(),
            })
            .unwrap();
    }
    database
}

fn clip(id: u128, event: u128, angle: u128) -> ReelClipSpec {
    ReelClipSpec {
        id: clip_id(id),
        event_id: EventId::new(nonzero(event)),
        angle: camera(angle),
        trim_in_nanos: 0,
        trim_out_nanos: 0,
        speed: ReelSpeed::REAL_TIME,
        reverse: false,
        transition_in: ReelTransition::Cut,
        program_audio: false,
    }
}

/// Clip 1: event 1 on camera 1 trimmed to 0.2..1 s at exactly 1/2x (80
/// frames). Clip 2: event 2 on camera 2 at 1x (50 frames) dissolving in over
/// 10 frames with program audio. 120 output frames at 50 fps.
fn two_clip_reel() -> HighlightReel {
    let mut reel = HighlightReel::new(ReelFrameRate::new(50, 1).unwrap());
    reel.push(ReelClipSpec {
        trim_in_nanos: 200_000_000,
        speed: ReelSpeed::new(1, 2).unwrap(),
        ..clip(1, 1, 1)
    })
    .unwrap();
    reel.push(ReelClipSpec {
        transition_in: ReelTransition::Dissolve { frames: 10 },
        program_audio: true,
        ..clip(2, 2, 2)
    })
    .unwrap();
    reel
}

#[test]
fn reel_compiles_trims_rational_speed_and_dissolve_into_exact_frames() {
    let compiled = two_clip_reel().compile(&events()).unwrap();
    let clips = compiled.clips();
    assert_eq!(compiled.total_frames(), 120);
    assert_eq!(clips[0].source, range(200 * MS, SECOND));
    assert_eq!((clips[0].output_start, clips[0].frame_count), (0, 80));
    assert_eq!((clips[1].output_start, clips[1].frame_count), (70, 50));
    assert_eq!(clips[1].transition_in_frames, 10);

    // Half speed advances 10 ms of source per 20 ms output frame.
    let plan = compiled.layers_at(3).unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].target, ts(230 * MS));
    assert_eq!(plan[0].weight, MixWeight::FULL);

    let first = compiled.layers_at(70).unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].clip, clip_id(1));
    assert_eq!(first[0].local_frame, 70);
    assert_eq!(first[0].target, ts(900 * MS));
    assert_eq!(
        (first[0].weight.numerator, first[0].weight.denominator),
        (10, 11)
    );
    assert_eq!(first[1].clip, clip_id(2));
    assert_eq!(first[1].target, ts(SECOND));
    assert_eq!(
        (first[1].weight.numerator, first[1].weight.denominator),
        (1, 11)
    );
    let last = compiled.layers_at(79).unwrap();
    assert_eq!(
        (last[0].weight.numerator, last[1].weight.numerator),
        (1, 10)
    );
    let after = compiled.layers_at(80).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].clip, clip_id(2));
    assert_eq!(after[0].local_frame, 10);
    assert_eq!(
        compiled.layers_at(120),
        Err(ReelError::FrameOutOfRange {
            frame: 120,
            total: 120
        })
    );
    assert_eq!(compiled.program_audio_frames(), vec![70..120]);
}

#[test]
fn reel_frames_resolve_to_exact_source_frames_and_segments() {
    let compiled = two_clip_reel().compile(&events()).unwrap();
    let timeline = source_timeline();
    let catalog = segment_catalog();

    let slow = compiled.resolve_frame(1, &timeline, &catalog).unwrap();
    let layer = slow.layers[0];
    assert_eq!(layer.plan.target, ts(210 * MS));
    assert_eq!(layer.observation.source_frame, FrameIndex::new(10));
    assert_eq!(layer.observation.synchronized_at, ts(201 * MS));
    assert_eq!(layer.offset_nanos, 9_000_000);
    assert_eq!(layer.segment_id, segment_id(1, 0));

    // Incoming camera 2 at exactly 1 s still shows its 0.982 s frame, which
    // lives in the previous segment.
    let dissolve = compiled.resolve_frame(70, &timeline, &catalog).unwrap();
    assert_eq!(
        dissolve.layers[0].observation.source_frame,
        FrameIndex::new(44)
    );
    assert_eq!(
        dissolve.layers[1].observation.source_frame,
        FrameIndex::new(49)
    );
    assert_eq!(dissolve.layers[1].segment_id, segment_id(2, 0));
    let settled = compiled.resolve_frame(71, &timeline, &catalog).unwrap();
    assert_eq!(settled.layers[1].segment_id, segment_id(2, 1));
}

#[test]
fn reel_rational_timing_is_exact_for_fractional_rates_and_speeds() {
    let mut reel = HighlightReel::new(ReelFrameRate::new(60_000, 2_002).unwrap());
    assert_eq!(reel.frame_rate().numerator(), 30_000);
    assert_eq!(reel.frame_rate().denominator(), 1_001);
    reel.push(ReelClipSpec {
        speed: ReelSpeed::new(4, 6).unwrap(),
        ..clip(1, 1, 1)
    })
    .unwrap();
    let compiled = reel.compile(&events()).unwrap();
    let clip = compiled.clips()[0];
    assert_eq!(clip.speed, ReelSpeed::new(2, 3).unwrap());
    // ceil(1 s * 30000/1001 * 3/2) frames.
    assert_eq!(clip.frame_count, 45);
    // floor(1 * 1001/30000 s * 2/3) = 22_244_444 ns.
    assert_eq!(compiled.layers_at(1).unwrap()[0].target, ts(22_244_444));
    assert_eq!(compiled.frame_start_sample(1, 48_000), 1_601);
    assert_eq!(compiled.frame_start_sample(5, 48_000), 8_008);
    assert_eq!(compiled.total_samples(48_000), 72_072);
}

#[test]
fn reel_rejects_invalid_edits_with_explicit_errors() {
    let database = events();
    let rate = ReelFrameRate::new(50, 1).unwrap();
    assert!(matches!(
        ReelFrameRate::new(241, 1),
        Err(ReelError::InvalidFrameRate { .. })
    ));
    assert!(matches!(
        ReelSpeed::new(17, 1),
        Err(ReelError::InvalidSpeed { .. })
    ));
    assert!(matches!(
        ReelSpeed::new(1, 17),
        Err(ReelError::InvalidSpeed { .. })
    ));
    assert_eq!(
        HighlightReel::new(rate).compile(&database),
        Err(ReelError::EmptyReel)
    );

    let mut reel = HighlightReel::new(rate);
    reel.push(clip(1, 1, 1)).unwrap();
    assert_eq!(
        reel.push(clip(1, 2, 1)),
        Err(ReelError::DuplicateClip(clip_id(1)))
    );

    let mut first_dissolve = HighlightReel::new(rate);
    first_dissolve
        .push(ReelClipSpec {
            transition_in: ReelTransition::Dissolve { frames: 5 },
            ..clip(1, 1, 1)
        })
        .unwrap();
    assert_eq!(
        first_dissolve.compile(&database),
        Err(ReelError::FirstClipTransition(clip_id(1)))
    );

    let mut too_long = HighlightReel::new(rate);
    too_long.push(clip(1, 1, 1)).unwrap();
    too_long
        .push(ReelClipSpec {
            transition_in: ReelTransition::Dissolve { frames: 51 },
            ..clip(2, 2, 2)
        })
        .unwrap();
    assert_eq!(
        too_long.compile(&database),
        Err(ReelError::TransitionTooLong {
            clip: clip_id(2),
            frames: 51,
            available: 50,
        })
    );

    let mut zero = HighlightReel::new(rate);
    zero.push(clip(1, 1, 1)).unwrap();
    zero.push(ReelClipSpec {
        transition_in: ReelTransition::Dissolve { frames: 0 },
        ..clip(2, 2, 2)
    })
    .unwrap();
    assert_eq!(
        zero.compile(&database),
        Err(ReelError::InvalidTransition {
            clip: clip_id(2),
            frames: 0
        })
    );

    let mut trimmed = HighlightReel::new(rate);
    trimmed
        .push(ReelClipSpec {
            trim_in_nanos: 600_000_000,
            trim_out_nanos: 400_000_000,
            ..clip(1, 1, 1)
        })
        .unwrap();
    assert_eq!(
        trimmed.compile(&database),
        Err(ReelError::TrimLeavesNoMedia(clip_id(1)))
    );

    let mut wrong_angle = HighlightReel::new(rate);
    wrong_angle.push(clip(1, 1, 7)).unwrap();
    assert_eq!(
        wrong_angle.compile(&database),
        Err(ReelError::AngleNotInEvent {
            event: EventId::new(nonzero(1)),
            camera: camera(7),
        })
    );

    let mut unknown = HighlightReel::new(rate);
    unknown.push(clip(1, 99, 1)).unwrap();
    assert_eq!(
        unknown.compile(&database),
        Err(ReelError::UnknownEvent(EventId::new(nonzero(99))))
    );
}

#[test]
fn reel_reorders_and_removes_clips() {
    let mut reel = two_clip_reel();
    reel.move_clip(clip_id(2), 0).unwrap();
    assert_eq!(reel.clips()[0].id, clip_id(2));
    // The dissolve now sits on the first clip and is refused at compile.
    assert_eq!(
        reel.compile(&events()),
        Err(ReelError::FirstClipTransition(clip_id(2)))
    );
    reel.remove(clip_id(2)).unwrap();
    assert_eq!(reel.compile(&events()).unwrap().total_frames(), 80);
    assert_eq!(
        reel.remove(clip_id(2)),
        Err(ReelError::UnknownClip(clip_id(2)))
    );
}

fn bed(tracks: Vec<MusicTrack>, repeat: BedRepeat) -> MusicBedSpec {
    MusicBedSpec {
        sample_rate: 48_000,
        tracks,
        repeat,
        crossfade_samples: 20,
        bed_gain: BedGain::UNITY,
        ducking: None,
        end_fade_samples: 0,
    }
}

/// A track whose duration reaches the top of the sample range must end
/// placement at the program end instead of overflowing the next start or the
/// fade-out position.
#[test]
fn music_bed_with_an_enormous_track_does_not_overflow() {
    for repeat in [BedRepeat::Once, BedRepeat::Loop] {
        let plan = MusicBedSpec {
            crossfade_samples: 0,
            ..bed(vec![track(1, 10), track(2, u64::MAX)], repeat)
        }
        .plan_samples(100, &[])
        .unwrap();
        let starts: Vec<_> = plan
            .entries()
            .iter()
            .map(|entry| (entry.output_start, entry.length_samples))
            .collect();
        assert_eq!(starts, vec![(0, 10), (10, 90)]);
        let voices = plan.voices_at(50);
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].entry_index, 1);
        assert_eq!(voices[0].source_sample, 40);
        assert_eq!(voices[0].gain.q16(), UNITY);
    }
}

#[test]
fn music_bed_loops_with_exact_crossfades() {
    let plan = bed(vec![track(1, 100), track(2, 60)], BedRepeat::Loop)
        .plan_samples(300, &[])
        .unwrap();
    let placed: Vec<_> = plan
        .entries()
        .iter()
        .map(|entry| {
            (
                entry.playlist_index,
                entry.iteration,
                entry.output_start,
                entry.length_samples,
            )
        })
        .collect();
    assert_eq!(
        placed,
        vec![
            (0, 0, 0, 100),
            (1, 0, 80, 60),
            (0, 1, 120, 100),
            (1, 1, 200, 60),
            (0, 2, 240, 60),
        ]
    );
    assert_eq!(plan.entries()[0].fade_in_samples, 0);
    assert_eq!(plan.entries()[4].fade_out_samples, 0);

    let voices = plan.voices_at(90);
    assert_eq!(voices.len(), 2);
    assert_eq!(voices[0].source_sample, 90);
    assert_eq!(voices[1].source_sample, 10);
    assert_eq!(voices[0].gain.q16(), UNITY / 2);
    assert_eq!(voices[1].gain.q16(), UNITY / 2);
    // 1/4 through the crossfade: floor ramps sum to unity.
    let voices = plan.voices_at(85);
    assert_eq!(
        (voices[0].gain.q16(), voices[1].gain.q16()),
        (49_152, 16_384)
    );
    assert_eq!(plan.voices_at(50).len(), 1);
    assert_eq!(plan.voices_at(300), Vec::new());

    let once = bed(vec![track(1, 100)], BedRepeat::Once)
        .plan_samples(300, &[])
        .unwrap();
    assert_eq!(once.entries().len(), 1);
    assert_eq!(once.bed_end(), 100);
    assert_eq!(once.voices_at(150), Vec::new());
}

#[test]
fn music_bed_ducks_under_program_audio_with_sample_exact_ramps() {
    let spec = MusicBedSpec {
        ducking: Some(DuckingSpec {
            ducked_gain: gain(UNITY / 4),
            attack_samples: 10,
            release_samples: 20,
        }),
        end_fade_samples: 100,
        ..bed(vec![track(1, 1_000)], BedRepeat::Once)
    };
    let plan = spec
        .plan_samples(300, std::slice::from_ref(&(150..200)))
        .unwrap();
    assert_eq!(plan.envelope_gain(139).q16(), UNITY);
    assert_eq!(plan.envelope_gain(140).q16(), UNITY);
    assert_eq!(plan.envelope_gain(145).q16(), 40_960);
    assert_eq!(plan.envelope_gain(150).q16(), UNITY / 4);
    assert_eq!(plan.envelope_gain(199).q16(), UNITY / 4);
    assert_eq!(plan.envelope_gain(210).q16(), 40_960);
    assert_eq!(plan.envelope_gain(220).q16(), UNITY);
    // End fade: unity at 200, half at 250, silent past the program.
    assert_eq!(plan.end_fade_gain(250).q16(), UNITY / 2);
    assert_eq!(plan.voices_at(250).first().unwrap().gain.q16(), UNITY / 2);
    assert_eq!(plan.voices_at(175)[0].gain.q16(), UNITY / 4);

    // Ducks closer than release + attack merge into one ducked span.
    let merged = spec.plan_samples(300, &[100..120, 140..160]).unwrap();
    assert_eq!(merged.envelope_gain(130).q16(), UNITY / 4);
    assert_eq!(merged.envelope().len(), 5);

    // From a compiled reel: clip 2 carries program audio over frames 70..120.
    let from_reel = MusicBedSpec {
        end_fade_samples: 0,
        ..spec
    }
    .plan(&two_clip_reel().compile(&events()).unwrap())
    .unwrap();
    assert_eq!(from_reel.program_samples(), 115_200);
    assert_eq!(from_reel.envelope_gain(67_189).q16(), UNITY);
    assert_eq!(from_reel.envelope_gain(67_200).q16(), UNITY / 4);
    assert_eq!(from_reel.envelope_gain(115_199).q16(), UNITY / 4);
}

#[test]
fn music_bed_rejects_invalid_plans() {
    assert_eq!(
        BedGain::from_q16(UNITY + 1),
        Err(MusicBedError::InvalidGain(UNITY + 1))
    );
    let base = bed(vec![track(1, 100)], BedRepeat::Loop);
    assert_eq!(base.plan_samples(0, &[]), Err(MusicBedError::EmptyProgram));
    assert_eq!(
        bed(Vec::new(), BedRepeat::Loop).plan_samples(10, &[]),
        Err(MusicBedError::NoTracks)
    );
    assert_eq!(
        bed(vec![track(1, 0)], BedRepeat::Loop).plan_samples(10, &[]),
        Err(MusicBedError::EmptyTrack(MusicTrackId::new(nonzero(1))))
    );
    assert!(matches!(
        bed(vec![track(1, 39)], BedRepeat::Loop).plan_samples(10, &[]),
        Err(MusicBedError::CrossfadeTooLong { .. })
    ));
    assert_eq!(
        MusicBedSpec {
            sample_rate: 1_000,
            ..base.clone()
        }
        .plan_samples(10, &[]),
        Err(MusicBedError::InvalidSampleRate(1_000))
    );
    assert_eq!(
        base.plan_samples(300, &[100..150, 140..160]),
        Err(MusicBedError::InvalidDuckInterval(140..160))
    );
    assert_eq!(
        base.plan_samples(300, std::slice::from_ref(&(290..301))),
        Err(MusicBedError::InvalidDuckInterval(290..301))
    );
    assert!(matches!(
        MusicBedSpec {
            end_fade_samples: 301,
            ..base.clone()
        }
        .plan_samples(300, &[]),
        Err(MusicBedError::EndFadeTooLong { .. })
    ));
    assert_eq!(
        MusicBedSpec {
            crossfade_samples: 0,
            ..bed(vec![track(1, 1)], BedRepeat::Loop)
        }
        .plan_samples(5_000, &[]),
        Err(MusicBedError::TooManyEntries {
            maximum: MAX_BED_ENTRIES
        })
    );
}

#[test]
fn export_during_record_protects_finalized_segments_and_progresses_by_frames() {
    let compiled = two_clip_reel().compile(&events()).unwrap();
    let timeline = source_timeline();
    let mut catalog = segment_catalog();
    let mut queue = RecordExportQueue::default();
    let job = queue
        .submit(job_id(1), &compiled, &mut catalog, &timeline)
        .unwrap();
    assert_eq!(job.state(), &RecordExportState::Queued);
    // Camera 2's lead frame lives in its first segment.
    let planned: Vec<_> = job.segments().collect();
    assert_eq!(
        planned,
        vec![segment_id(1, 0), segment_id(2, 0), segment_id(2, 1)]
    );
    let reference = ProtectionReference::Export(job_id(1));
    for id in &planned {
        assert!(
            catalog
                .segment(*id)
                .unwrap()
                .protections
                .contains(&reference)
        );
    }

    // Retention while the job runs reclaims only unreferenced closed media.
    let report = catalog.reclaim(root(), u64::MAX).unwrap();
    assert_eq!(report.deleted, vec![segment_id(1, 1)]);

    let chunk = queue
        .advance(job_id(1), 50, &mut catalog, &timeline)
        .unwrap();
    assert_eq!(chunk.frames, 0..50);
    assert_eq!(chunk.resolved.len(), 50);
    assert_eq!(chunk.segments, vec![segment_id(1, 0)]);
    assert_eq!(chunk.progress.completed_frames, 50);
    assert_eq!(chunk.progress.permille(), 416);
    assert!(!chunk.completed);
    assert_eq!(
        queue.job(job_id(1)).unwrap().state(),
        &RecordExportState::Running
    );

    let chunk = queue
        .advance(job_id(1), 50, &mut catalog, &timeline)
        .unwrap();
    assert_eq!(chunk.frames, 50..100);
    assert_eq!(
        chunk.segments,
        vec![segment_id(1, 0), segment_id(2, 0), segment_id(2, 1)]
    );
    let chunk = queue
        .advance(job_id(1), MAX_EXPORT_CHUNK_FRAMES, &mut catalog, &timeline)
        .unwrap();
    assert_eq!(chunk.frames, 100..120);
    assert!(chunk.completed);
    assert_eq!(chunk.progress.permille(), 1_000);
    assert_eq!(
        queue.job(job_id(1)).unwrap().state(),
        &RecordExportState::Completed
    );
    for id in &planned {
        assert!(catalog.segment(*id).unwrap().protections.is_empty());
    }
    assert_eq!(
        queue.advance(job_id(1), 1, &mut catalog, &timeline),
        Err(RecordExportError::NotActive(job_id(1)))
    );
    assert_eq!(
        queue.advance(job_id(1), 0, &mut catalog, &timeline),
        Err(RecordExportError::InvalidChunk {
            requested: 0,
            maximum: MAX_EXPORT_CHUNK_FRAMES
        })
    );
    assert!(queue.forget(job_id(1)).is_ok());
}

#[test]
fn export_refuses_open_segments_gaps_and_duplicates_atomically() {
    let timeline = source_timeline();
    let mut catalog = segment_catalog();
    let mut queue = RecordExportQueue::default();
    let rate = ReelFrameRate::new(50, 1).unwrap();

    let mut recording = HighlightReel::new(rate);
    recording.push(clip(1, 2, 2)).unwrap();
    recording.push(clip(2, 3, 1)).unwrap();
    let recording = recording.compile(&events()).unwrap();
    assert_eq!(
        queue.submit(job_id(1), &recording, &mut catalog, &timeline),
        Err(RecordExportError::SegmentNotFinalized {
            clip: clip_id(2),
            segment: segment_id(1, 2),
        })
    );
    // Nothing was protected for the refused job.
    assert!(
        catalog
            .segments()
            .all(|segment| segment.protections.is_empty())
    );
    assert!(queue.job(job_id(1)).is_none());

    let mut gap = HighlightReel::new(rate);
    gap.push(clip(1, 4, 1)).unwrap();
    let gap = gap.compile(&events()).unwrap();
    assert_eq!(
        queue.submit(job_id(1), &gap, &mut catalog, &timeline),
        Err(RecordExportError::CoverageGap {
            clip: clip_id(1),
            gap: range(3 * SECOND, 3 * SECOND + 500 * MS),
        })
    );

    let compiled = two_clip_reel().compile(&events()).unwrap();
    queue
        .submit(job_id(1), &compiled, &mut catalog, &timeline)
        .unwrap();
    assert_eq!(
        queue.submit(job_id(1), &compiled, &mut catalog, &timeline),
        Err(RecordExportError::DuplicateJob(job_id(1)))
    );
}

#[test]
fn export_cancellation_releases_protection() {
    let compiled = two_clip_reel().compile(&events()).unwrap();
    let timeline = source_timeline();
    let mut catalog = segment_catalog();
    let mut queue = RecordExportQueue::default();
    queue
        .submit(job_id(1), &compiled, &mut catalog, &timeline)
        .unwrap();
    queue
        .submit(job_id(2), &compiled, &mut catalog, &timeline)
        .unwrap();
    queue
        .advance(job_id(1), 10, &mut catalog, &timeline)
        .unwrap();
    assert_eq!(
        queue.forget(job_id(1)).unwrap_err(),
        RecordExportError::StillActive(job_id(1))
    );
    queue.cancel(job_id(1), &mut catalog).unwrap();
    assert_eq!(
        queue.job(job_id(1)).unwrap().state(),
        &RecordExportState::Cancelled
    );
    assert_eq!(
        queue.cancel(job_id(1), &mut catalog),
        Err(RecordExportError::NotActive(job_id(1)))
    );
    let protections = &catalog.segment(segment_id(1, 0)).unwrap().protections;
    assert!(!protections.contains(&ProtectionReference::Export(job_id(1))));
    assert!(protections.contains(&ProtectionReference::Export(job_id(2))));
    // The other job still completes.
    let chunk = queue
        .advance(job_id(2), MAX_EXPORT_CHUNK_FRAMES, &mut catalog, &timeline)
        .unwrap();
    assert!(chunk.completed);
}

#[test]
fn export_never_references_pruned_media() {
    let compiled = two_clip_reel().compile(&events()).unwrap();
    let timeline = source_timeline();
    let mut catalog = segment_catalog();
    let mut queue = RecordExportQueue::default();
    queue
        .submit(job_id(1), &compiled, &mut catalog, &timeline)
        .unwrap();
    queue
        .advance(job_id(1), 10, &mut catalog, &timeline)
        .unwrap();

    // A caller drops the protection and retention prunes the media.
    catalog.release_reference(ProtectionReference::Export(job_id(1)));
    let report = catalog.reclaim(root(), u64::MAX).unwrap();
    assert!(report.deleted.contains(&segment_id(1, 0)));
    let failures = queue.revalidate(&mut catalog, &timeline);
    assert_eq!(
        failures,
        vec![(
            job_id(1),
            RecordExportFailure::SegmentPruned(segment_id(1, 0))
        )]
    );
    assert!(matches!(
        queue.job(job_id(1)).unwrap().state(),
        RecordExportState::Failed(RecordExportFailure::SegmentPruned(_))
    ));
    assert_eq!(
        queue.advance(job_id(1), 1, &mut catalog, &timeline),
        Err(RecordExportError::NotActive(job_id(1)))
    );

    // Observation pruning inside a clip fails the next chunk.
    let mut catalog = segment_catalog();
    let mut timeline = source_timeline();
    queue
        .submit(job_id(2), &compiled, &mut catalog, &timeline)
        .unwrap();
    timeline.prune_before(ts(500 * MS));
    assert_eq!(
        queue.advance(job_id(2), 10, &mut catalog, &timeline),
        Err(RecordExportError::Failed {
            job: job_id(2),
            failure: RecordExportFailure::SourceFramesPruned(clip_id(1)),
        })
    );
    assert!(
        catalog
            .segments()
            .all(|segment| segment.protections.is_empty())
    );
    // Resubmitting against the pruned timeline is refused up front.
    assert_eq!(
        queue
            .submit(job_id(3), &compiled, &mut catalog, &timeline)
            .unwrap_err(),
        RecordExportError::Unresolved {
            clip: clip_id(1),
            error: ReplayError::ObservationUnavailable(camera(1)),
        }
    );
}

#[test]
fn a_reverse_clip_plays_its_trimmed_range_backward() {
    let mut reel = HighlightReel::new(ReelFrameRate::new(50, 1).unwrap());
    reel.push(ReelClipSpec {
        reverse: true,
        ..clip(1, 1, 1)
    })
    .unwrap();
    let compiled = reel.compile(&events()).unwrap();
    let clips = compiled.clips();
    assert!(clips[0].reverse);
    // The clip keeps the same frame count; only its source times run from
    // the range's last frame toward its first.
    assert_eq!(clips[0].frame_count, 50);
    let first = compiled.layers_at(0).unwrap()[0].target;
    let second = compiled.layers_at(1).unwrap()[0].target;
    let last = compiled.layers_at(49).unwrap()[0].target;
    assert!(first > second && second > last);
    assert_eq!(first, ts(SECOND - 1));
    assert_eq!(last, ts(FRAME - 1));
    assert_eq!(compiled.layers_at(50), Err(ReelError::FrameOutOfRange {
        frame: 50,
        total: 50,
    }));
}
