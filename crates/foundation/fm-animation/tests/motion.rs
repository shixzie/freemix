//! Saved motions (user animation presets): capture one phase of one target,
//! retime it, size its crop to another box, and apply it to another target.

use fm_animation::{
    AnimationPreset, Clip, ClipError, Easing, Keyframe, MAX_MOTION_KEYS, MotionError, MotionTarget,
    Phase, PhaseMotion, PresetGeometry, Property, Timebase, Track, TrackKey, Value,
};
use fm_types::FrameRate;

const GEOMETRY: PresetGeometry = PresetGeometry {
    x: 100,
    y: 50,
    width: 400,
    height: 100,
    canvas_width: 1_920,
    canvas_height: 1_080,
};

fn rate(numerator: u32) -> Timebase {
    Timebase::new(FrameRate::new(numerator, 1).unwrap())
}

fn source() -> Clip<u32> {
    let mut clip = Clip::new(rate(30), 20, 10).unwrap();
    clip.apply_preset(1, Phase::In, AnimationPreset::Wipe, 20, GEOMETRY)
        .unwrap();
    clip.apply_preset(1, Phase::Out, AnimationPreset::Fade, 10, GEOMETRY)
        .unwrap();
    clip.apply_preset(2, Phase::In, AnimationPreset::Pop, 10, GEOMETRY)
        .unwrap();
    clip
}

fn scalars(keys: &[Keyframe]) -> Vec<(u32, i64)> {
    keys.iter()
        .map(|key| match key.value {
            Value::Scalar(value) => (key.frame, value),
            Value::Color(_) => panic!("scalar expected"),
        })
        .collect()
}

#[test]
fn capture_keeps_one_phase_of_one_target() {
    let motion = PhaseMotion::capture(&source(), 1, Phase::In, 400, 100).unwrap();
    assert_eq!(motion.frames(), 20);
    assert_eq!(motion.timebase(), rate(30));
    assert_eq!(
        motion.properties().collect::<Vec<_>>(),
        [Property::CropRight]
    );
    assert!(
        motion
            .clip()
            .tracks()
            .iter()
            .all(|track| track.key.phase == Phase::In && track.key.target == MotionTarget)
    );
    let out = PhaseMotion::capture(&source(), 1, Phase::Out, 400, 100).unwrap();
    assert_eq!(out.frames(), 10);
    assert_eq!(out.properties().collect::<Vec<_>>(), [Property::Opacity]);
    // Element 2 has no Out phase tracks; element 3 has nothing at all.
    assert_eq!(
        PhaseMotion::capture(&source(), 2, Phase::Out, 1, 1),
        Err(MotionError::Empty)
    );
    assert_eq!(
        PhaseMotion::capture(&source(), 1, Phase::In, 0, 100),
        Err(MotionError::ZeroReference)
    );
}

#[test]
fn new_refuses_out_phases_and_oversized_motions() {
    let out_track = Track::new(
        TrackKey::new(MotionTarget, Phase::Out, Property::Opacity),
        vec![Keyframe::new(0, Value::Scalar(0), Easing::Linear)],
    );
    let clip = Clip::from_tracks(rate(30), 0, 5, vec![out_track]).unwrap();
    assert_eq!(
        PhaseMotion::new(clip, 10, 10),
        Err(MotionError::NotOnePhase)
    );
    let keys = (0..256_u32)
        .map(|frame| Keyframe::new(frame, Value::Scalar(i64::from(frame)), Easing::Linear))
        .collect::<Vec<_>>();
    let tracks = [
        Property::PositionX,
        Property::PositionY,
        Property::Rotation,
        Property::CropLeft,
        Property::CropTop,
    ]
    .into_iter()
    .map(|property| {
        Track::new(
            TrackKey::new(MotionTarget, Phase::In, property),
            keys.clone(),
        )
    })
    .collect();
    let clip = Clip::from_tracks(rate(30), 300, 0, tracks).unwrap();
    assert_eq!(
        PhaseMotion::new(clip, 10, 10),
        Err(MotionError::TooManyKeys { count: 5 * 256 })
    );
    const { assert!(5 * 256 > MAX_MOTION_KEYS) };
    let empty = Clip::new(rate(30), 10, 0).unwrap();
    assert_eq!(PhaseMotion::new(empty, 10, 10), Err(MotionError::Empty));
}

#[test]
fn applied_motion_is_retimed_and_sized_to_the_new_box() {
    let motion = PhaseMotion::capture(&source(), 1, Phase::In, 400, 100).unwrap();
    // On an 800 px wide layer over 40 frames: the wipe covers the whole box.
    let mut target = Clip::<u8>::new(rate(30), 5, 0).unwrap();
    let put = target
        .apply_motion(7, Phase::Out, &motion, 40, 800, 50, |_| true)
        .unwrap();
    assert_eq!(put, 1);
    assert_eq!(target.out_frames(), 40);
    assert_eq!(target.in_frames(), 5);
    let track = target
        .track(TrackKey::new(7, Phase::Out, Property::CropRight))
        .unwrap();
    assert_eq!(scalars(&track.keys), [(0, 800), (40, 0)]);
    assert_eq!(track.keys[0].easing, Easing::CubicInOut);
    // A longer phase stays as long; keys land at the requested length.
    let mut long = Clip::<u8>::new(rate(30), 60, 0).unwrap();
    long.apply_motion(7, Phase::In, &motion, 10, 400, 100, |_| true)
        .unwrap();
    assert_eq!(long.in_frames(), 60);
    let track = long
        .track(TrackKey::new(7, Phase::In, Property::CropRight))
        .unwrap();
    assert_eq!(scalars(&track.keys), [(0, 400), (10, 0)]);
}

#[test]
fn apply_skips_properties_the_target_cannot_animate() {
    let motion = PhaseMotion::capture(&source(), 2, Phase::In, 400, 100).unwrap();
    let mut target = Clip::<u8>::new(rate(30), 0, 0).unwrap();
    let put = target
        .apply_motion(1, Phase::In, &motion, 10, 1, 1, |property| {
            property == Property::ScaleY
        })
        .unwrap();
    assert_eq!(put, 1);
    assert_eq!(target.tracks().len(), 1);
    assert_eq!(target.tracks()[0].key.property, Property::ScaleY);
}

#[test]
fn a_collapsing_retime_is_refused_and_changes_nothing() {
    let keys = (0..=4)
        .map(|frame| Keyframe::new(frame, Value::Scalar(i64::from(frame)), Easing::Linear))
        .collect();
    let clip = Clip::from_tracks(
        rate(30),
        4,
        0,
        vec![Track::new(
            TrackKey::new(MotionTarget, Phase::In, Property::PositionX),
            keys,
        )],
    )
    .unwrap();
    let motion = PhaseMotion::new(clip, 10, 10).unwrap();
    let mut target = Clip::<u8>::new(rate(30), 0, 0).unwrap();
    let before = target.clone();
    assert_eq!(
        target.apply_motion(3, Phase::Out, &motion, 2, 10, 10, |_| true),
        Err(ClipError::RetimeCollision {
            track: TrackKey::new(3, Phase::Out, Property::PositionX),
            min_len: 4,
        })
    );
    assert_eq!(target, before);
}

#[test]
fn frames_at_converts_the_duration_between_timebases() {
    let motion = PhaseMotion::capture(&source(), 1, Phase::In, 400, 100).unwrap();
    assert_eq!(motion.frames_at(rate(30)), 20);
    assert_eq!(motion.frames_at(rate(60)), 40);
    // 20 frames at 30 fps is 16.67 frames at 25 fps: rounded half up.
    assert_eq!(motion.frames_at(rate(25)), 17);
    let ntsc = Timebase::new(FrameRate::new(30_000, 1_001).unwrap());
    assert_eq!(motion.frames_at(ntsc), 20);
}
