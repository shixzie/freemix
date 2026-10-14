use fm_animation::{
    AnimationPreset, Clip, ClipError, ClipPosition, ClipState, Easing, Phase, PresetGeometry,
    Property, Timebase, TrackKey, Value,
};
use fm_types::FrameRate;
use std::collections::BTreeSet;

const GEOMETRY: PresetGeometry = PresetGeometry {
    x: 100,
    y: 50,
    width: 200,
    height: 80,
    canvas_width: 1_920,
    canvas_height: 1_080,
};

fn clip() -> Clip<u32> {
    Clip::new(Timebase::new(FrameRate::new(50, 1).unwrap()), 25, 25).unwrap()
}

fn at(clip: &Clip<u32>, property: Property, state: ClipState) -> i64 {
    match clip.sample(1, property, state) {
        Some(Value::Scalar(value)) => value,
        other => panic!("{property:?} {state:?}: {other:?}"),
    }
}

fn in_at(frame: u32) -> ClipState {
    ClipState::In(ClipPosition::frame(frame))
}

fn out_at(frame: u32) -> ClipState {
    ClipState::Out(ClipPosition::frame(frame))
}

#[test]
fn every_in_preset_ends_on_the_static_design() {
    for preset in AnimationPreset::ALL {
        let mut clip = clip();
        clip.apply_preset(1, Phase::In, preset, 20, GEOMETRY)
            .unwrap();
        clip.apply_preset(1, Phase::Out, preset, 20, GEOMETRY)
            .unwrap();
        let properties: Vec<Property> = clip
            .tracks()
            .iter()
            .map(|track| track.key.property)
            .collect();
        assert_ne!(properties, []);
        for property in properties {
            let identity = property.identity().unwrap();
            assert_eq!(
                clip.sample(1, property, ClipState::Hold),
                Some(identity),
                "{preset:?} {property:?} hold"
            );
            assert_eq!(
                clip.sample(1, property, out_at(0)),
                Some(identity),
                "{preset:?} {property:?} out start"
            );
        }
    }
}

#[test]
fn preset_ids_round_trip() {
    let ids: BTreeSet<&str> = AnimationPreset::ALL
        .iter()
        .map(|preset| preset.id())
        .collect();
    assert_eq!(ids.len(), AnimationPreset::ALL.len());
    for preset in AnimationPreset::ALL {
        assert_eq!(AnimationPreset::from_id(preset.id()), Some(preset));
    }
}

#[test]
fn fade_runs_opacity_from_and_to_transparent() {
    let mut clip = clip();
    clip.apply_preset(1, Phase::In, AnimationPreset::Fade, 10, GEOMETRY)
        .unwrap();
    clip.apply_preset(1, Phase::Out, AnimationPreset::Fade, 10, GEOMETRY)
        .unwrap();
    assert_eq!(at(&clip, Property::Opacity, in_at(0)), 0);
    assert_eq!(at(&clip, Property::Opacity, in_at(5)), 127);
    assert_eq!(at(&clip, Property::Opacity, in_at(10)), 255);
    assert_eq!(at(&clip, Property::Opacity, out_at(10)), 0);
}

#[test]
fn slides_start_fully_off_canvas() {
    let cases = [
        (AnimationPreset::SlideFromLeft, Property::PositionX, -300),
        (AnimationPreset::SlideFromRight, Property::PositionX, 1_820),
        (AnimationPreset::SlideFromTop, Property::PositionY, -130),
        (AnimationPreset::SlideFromBottom, Property::PositionY, 1_030),
    ];
    for (preset, property, offscreen) in cases {
        let mut clip = clip();
        clip.apply_preset(1, Phase::In, preset, 12, GEOMETRY)
            .unwrap();
        clip.apply_preset(1, Phase::Out, preset, 12, GEOMETRY)
            .unwrap();
        assert_eq!(at(&clip, property, in_at(0)), offscreen, "{preset:?}");
        assert_eq!(at(&clip, property, in_at(12)), 0, "{preset:?}");
        assert_eq!(at(&clip, property, out_at(12)), offscreen, "{preset:?}");
        let track = clip.track(TrackKey::new(1, Phase::In, property)).unwrap();
        assert_eq!(track.keys[0].easing, Easing::CubicOut);
    }
}

#[test]
fn pop_overshoots_with_back_and_settles_at_full_scale() {
    let mut clip = clip();
    clip.apply_preset(1, Phase::In, AnimationPreset::Pop, 20, GEOMETRY)
        .unwrap();
    for property in [Property::ScaleX, Property::ScaleY] {
        assert_eq!(at(&clip, property, in_at(0)), 0);
        let peak = (0..=20)
            .map(|frame| at(&clip, property, in_at(frame)))
            .max()
            .unwrap();
        assert!(peak > 10_000, "{property:?} peak {peak}");
        assert_eq!(at(&clip, property, in_at(20)), 10_000);
    }
}

#[test]
fn wipe_reveals_with_an_animated_crop() {
    let mut clip = clip();
    clip.apply_preset(1, Phase::In, AnimationPreset::Wipe, 10, GEOMETRY)
        .unwrap();
    assert_eq!(at(&clip, Property::CropRight, in_at(0)), 200);
    assert_eq!(at(&clip, Property::CropRight, in_at(5)), 100);
    assert_eq!(at(&clip, Property::CropRight, in_at(10)), 0);
}

#[test]
fn drop_bounces_in_from_above_and_falls_out_below() {
    let mut clip = clip();
    clip.apply_preset(1, Phase::In, AnimationPreset::Drop, 20, GEOMETRY)
        .unwrap();
    clip.apply_preset(1, Phase::Out, AnimationPreset::Drop, 20, GEOMETRY)
        .unwrap();
    assert_eq!(at(&clip, Property::PositionY, in_at(0)), -130);
    let path: Vec<i64> = (0..=20)
        .map(|frame| at(&clip, Property::PositionY, in_at(frame)))
        .collect();
    // It bounces: on the way down it moves back up at least once.
    assert!(path.windows(2).any(|pair| pair[1] < pair[0]), "{path:?}");
    assert_eq!(path[20], 0);
    assert_eq!(at(&clip, Property::PositionY, out_at(20)), 1_030);
}

#[test]
fn presets_respect_the_phase_and_cut_at_zero_duration() {
    let mut clip = clip();
    assert_eq!(
        clip.apply_preset(1, Phase::In, AnimationPreset::Fade, 26, GEOMETRY),
        Err(ClipError::KeyOutsidePhase {
            track: TrackKey::new(1, Phase::In, Property::Opacity),
            frame: 26
        })
    );
    assert_eq!(clip.tracks(), []);
    clip.apply_preset(1, Phase::In, AnimationPreset::Fade, 0, GEOMETRY)
        .unwrap();
    assert_eq!(at(&clip, Property::Opacity, in_at(0)), 255);
}
