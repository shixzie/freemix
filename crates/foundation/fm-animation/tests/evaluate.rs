use fm_animation::{
    Clip, ClipPosition, ClipState, Easing, Keyframe, Phase, Property, Rgba, Timebase, Track,
    TrackKey, Value, sample_frame,
};
use fm_types::FrameRate;

fn rate(numerator: u32, denominator: u32) -> FrameRate {
    FrameRate::new(numerator, denominator).unwrap()
}

fn scalar(frame: u32, value: i64, easing: Easing) -> Keyframe {
    Keyframe::new(frame, Value::Scalar(value), easing)
}

fn clip_with(rate: FrameRate, in_frames: u32, track: Track<u32>) -> Clip<u32> {
    Clip::from_tracks(Timebase::new(rate), in_frames, 10, vec![track]).unwrap()
}

#[test]
fn scalar_sampling_matches_the_titles_contract() {
    let keys = [
        Keyframe::new(4, -20_i64, Easing::QuadOut),
        Keyframe::new(14, 0, Easing::Linear),
        Keyframe::new(20, 7, Easing::Linear),
    ];
    assert_eq!(sample_frame(&keys, 0), Some(-20));
    assert_eq!(sample_frame(&keys, 4), Some(-20));
    // QuadOut at 0.5: 0.75 -> -20 + trunc(20 * 49152 / 65536) = -5.
    assert_eq!(sample_frame(&keys, 9), Some(-5));
    assert_eq!(sample_frame(&keys, 14), Some(0));
    assert_eq!(sample_frame(&keys, 17), Some(3));
    assert_eq!(sample_frame(&keys, 99), Some(7));
    assert_eq!(sample_frame::<i64>(&[], 3), None);
}

#[test]
fn hold_keys_keep_their_value_until_the_next_key() {
    let keys = [
        Keyframe::new(0, 10_i64, Easing::Hold),
        Keyframe::new(10, 50, Easing::Linear),
        Keyframe::new(20, 90, Easing::Hold),
        Keyframe::new(30, 0, Easing::Linear),
    ];
    for frame in 0..10 {
        assert_eq!(sample_frame(&keys, frame), Some(10), "{frame}");
    }
    assert_eq!(sample_frame(&keys, 10), Some(50));
    assert_eq!(sample_frame(&keys, 15), Some(70));
    for frame in 20..30 {
        assert_eq!(sample_frame(&keys, frame), Some(90), "{frame}");
    }
    assert_eq!(sample_frame(&keys, 30), Some(0));
}

#[test]
fn a_25p_clip_plays_exactly_on_a_50p_project() {
    let key = TrackKey::new(1, Phase::In, Property::PositionX);
    let track = Track::new(
        key,
        vec![
            scalar(0, 0, Easing::Linear),
            scalar(10, 100, Easing::Linear),
        ],
    );
    let clip = clip_with(rate(25, 1), 10, track);
    let at = |frame| {
        clip.sample_project(1, Property::PositionX, Phase::In, frame, rate(50, 1))
            .unwrap()
    };
    // The 25p clip on 50p is exactly the same animation authored at 50p
    // with every key frame doubled, including the odd (half) frames.
    let doubled = [
        Keyframe::new(0, 0_i64, Easing::Linear),
        Keyframe::new(20, 100, Easing::Linear),
    ];
    for frame in 0..=20_u32 {
        assert_eq!(
            at(u64::from(frame)),
            Value::Scalar(sample_frame(&doubled, frame).unwrap()),
            "{frame}"
        );
    }
    // Even project frames land on whole clip frames.
    let native = [
        Keyframe::new(0, 0_i64, Easing::Linear),
        Keyframe::new(10, 100, Easing::Linear),
    ];
    for frame in 0..=10_u32 {
        assert_eq!(
            at(u64::from(2 * frame)),
            Value::Scalar(sample_frame(&native, frame).unwrap())
        );
    }
    // Half a clip frame of 10 is Q16 progress floor(65536 / 20) = 3276, and
    // 100 * 3276 / 65536 truncates to 4.
    assert_eq!(at(1), Value::Scalar(4));
    assert_eq!(at(20), Value::Scalar(100));
    assert_eq!(at(400), Value::Scalar(100));
    assert_eq!(
        clip.timebase().position_at(3, rate(50, 1)),
        ClipPosition::new(3, 2)
    );
}

#[test]
fn matching_rates_sample_exactly_like_whole_frames() {
    for (numerator, denominator) in [(25, 1), (30_000, 1_001), (60, 1)] {
        let rate = rate(numerator, denominator);
        let key = TrackKey::new(7, Phase::Out, Property::Rotation);
        let keys = vec![
            scalar(0, -3_000, Easing::CubicInOut),
            scalar(13, 45_000, Easing::BackOut),
            scalar(40, 9_000, Easing::BounceOut),
            scalar(60, 0, Easing::Linear),
        ];
        let track = Track::new(key, keys.clone());
        let clip = Clip::from_tracks(Timebase::new(rate), 0, 60, vec![track]).unwrap();
        for frame in 0..=60 {
            assert_eq!(
                clip.sample_project(7, Property::Rotation, Phase::Out, u64::from(frame), rate),
                sample_frame(&keys, frame),
                "{numerator}/{denominator} frame {frame}"
            );
        }
    }
}

#[test]
fn ntsc_clip_on_integer_project_converts_exactly() {
    // A 30000/1001 clip on a 30p project: project frame 1001 is clip frame
    // 1000 exactly.
    let timebase = Timebase::new(rate(30_000, 1_001));
    assert_eq!(
        timebase.position_at(1_001, rate(30, 1)),
        ClipPosition::frame(1_000)
    );
    assert_eq!(
        timebase.position_at(1, rate(30, 1)),
        ClipPosition::new(1_000, 1_001)
    );
}

#[test]
fn hold_state_samples_in_at_its_last_frame_and_out_runs_separately() {
    let in_key = TrackKey::new(1, Phase::In, Property::Opacity);
    let out_key = TrackKey::new(1, Phase::Out, Property::Opacity);
    let clip = Clip::from_tracks(
        Timebase::new(rate(50, 1)),
        12,
        8,
        vec![
            Track::new(
                in_key,
                vec![scalar(0, 0, Easing::Linear), scalar(8, 200, Easing::Linear)],
            ),
            Track::new(
                out_key,
                vec![scalar(0, 255, Easing::Linear), scalar(8, 0, Easing::Linear)],
            ),
        ],
    )
    .unwrap();
    assert_eq!(
        clip.sample_hold(1, Property::Opacity),
        Some(Value::Scalar(200))
    );
    assert_eq!(
        clip.sample(1, Property::Opacity, ClipState::Hold),
        clip.sample(1, Property::Opacity, ClipState::In(ClipPosition::frame(12)))
    );
    assert_eq!(clip.sample(1, Property::Opacity, ClipState::Static), None);
    assert_eq!(
        clip.sample(1, Property::Opacity, ClipState::Out(ClipPosition::frame(4))),
        Some(Value::Scalar(128))
    );
    assert_eq!(clip.sample(2, Property::Opacity, ClipState::Hold), None);
    assert_eq!(
        clip.evaluate(1, ClipState::Hold),
        vec![(Property::Opacity, Value::Scalar(200))]
    );
}

#[test]
fn overshoot_is_clamped_to_the_property_range() {
    let key = TrackKey::new(1, Phase::In, Property::Opacity);
    let track = Track::new(
        key,
        vec![
            scalar(0, 0, Easing::BackOut),
            scalar(20, 255, Easing::Linear),
        ],
    );
    let clip = clip_with(rate(25, 1), 20, track.clone());
    let raw_max = (0..=20)
        .filter_map(|frame| match track.sample(ClipPosition::frame(frame)) {
            Some(Value::Scalar(value)) => Some(value),
            _ => None,
        })
        .max()
        .unwrap();
    assert!(raw_max > 255);
    for frame in 0..=20 {
        let Some(Value::Scalar(value)) = clip.sample(
            1,
            Property::Opacity,
            ClipState::In(ClipPosition::frame(frame)),
        ) else {
            panic!("missing sample");
        };
        assert!((0..=255).contains(&value));
    }
}

#[test]
fn color_interpolates_per_channel_rounding_half_up_once() {
    let key = TrackKey::new(1, Phase::In, Property::Color);
    let track = Track::new(
        key,
        vec![
            Keyframe::new(0, Value::Color(Rgba::new(0, 255, 10, 0)), Easing::Linear),
            Keyframe::new(2, Value::Color(Rgba::new(255, 0, 11, 255)), Easing::Linear),
        ],
    );
    let clip = clip_with(rate(25, 1), 2, track);
    assert_eq!(
        clip.sample(1, Property::Color, ClipState::In(ClipPosition::frame(1))),
        Some(Value::Color(Rgba::new(128, 128, 11, 128)))
    );
}

#[test]
fn values_layer_onto_static_values() {
    assert_eq!(
        Property::PositionX.layer_scalar(100, Value::Scalar(-30)),
        70
    );
    // 200 px at 1.5x.
    assert_eq!(
        Property::ScaleX.layer_scalar(200, Value::Scalar(15_000)),
        300
    );
    // Opacity 200 * 128 / 255 = 100.39 -> 100.
    assert_eq!(Property::Opacity.layer_scalar(200, Value::Scalar(128)), 100);
    assert_eq!(Property::Opacity.layer_scalar(201, Value::Scalar(255)), 201);
    let base = Rgba::new(1, 2, 3, 4);
    let over = Rgba::new(9, 9, 9, 9);
    assert_eq!(Property::Color.layer_color(base, Value::Color(over)), over);
    assert_eq!(
        Property::Opacity.layer_color(base, Value::Color(over)),
        base
    );
    for property in Property::ALL {
        assert_eq!(Property::from_id(property.id()), Some(property));
        if let Some(identity) = property.identity() {
            assert_eq!(property.layer_scalar(123, identity), 123, "{property:?}");
        }
    }
}

#[test]
fn project_frames_is_the_first_frame_whose_position_reaches_the_phase_end() {
    let cases = [
        // (clip rate, project rate, clip frames, project frames)
        ((30, 1), (30, 1), 12, 12),
        ((30, 1), (60, 1), 10, 20),
        ((60, 1), (30, 1), 9, 5),
        ((25, 1), (30_000, 1_001), 25, 30),
        ((30, 1), (30_000, 1_001), 30, 30),
        ((30_000, 1_001), (30, 1), 1_000, 1_001),
        ((30, 1), (24, 1), 0, 0),
    ];
    for ((cn, cd), (pn, pd), clip_frames, expected) in cases {
        let timebase = Timebase::new(rate(cn, cd));
        let project = rate(pn, pd);
        let frames = timebase.project_frames(clip_frames, project);
        assert_eq!(frames, expected, "{cn}/{cd} on {pn}/{pd}");
        assert!(!timebase.position_at(frames, project).before(clip_frames));
        if frames > 0 {
            assert!(
                timebase
                    .position_at(frames - 1, project)
                    .before(clip_frames)
            );
        }
    }
}
