use fm_animation::{
    AnimationEdit, Clip, ClipError, Easing, KeySelection, Keyframe, Phase, Property, Timebase,
    Track, TrackKey, Value, round_half_up, stretch_frame,
};
use fm_types::FrameRate;

const X_IN: TrackKey<u32> = TrackKey::new(1, Phase::In, Property::PositionX);
const Y_IN: TrackKey<u32> = TrackKey::new(1, Phase::In, Property::PositionY);
const X_OUT: TrackKey<u32> = TrackKey::new(1, Phase::Out, Property::PositionX);

fn timebase() -> Timebase {
    Timebase::new(FrameRate::new(30, 1).unwrap())
}

fn track(key: TrackKey<u32>, frames: &[u32]) -> Track<u32> {
    Track::new(
        key,
        frames
            .iter()
            .map(|&frame| Keyframe::new(frame, Value::Scalar(i64::from(frame)), Easing::Linear))
            .collect(),
    )
}

fn frames(clip: &Clip<u32>, key: TrackKey<u32>) -> Vec<u32> {
    clip.track(key)
        .unwrap()
        .keys
        .iter()
        .map(|key| key.frame)
        .collect()
}

fn select(key: TrackKey<u32>, frames: &[u32]) -> KeySelection<u32> {
    let mut selection = KeySelection::new();
    for &frame in frames {
        selection.insert(key, frame);
    }
    selection
}

#[test]
fn rounding_is_half_up() {
    assert_eq!(round_half_up(5, 2), 3);
    assert_eq!(round_half_up(-5, 2), -2);
    assert_eq!(round_half_up(7, 3), 2);
    assert_eq!(stretch_frame(25, 30, 3), 3);
    assert_eq!(stretch_frame(5, 0, 10), 0);
}

#[test]
fn stretching_a_phase_scales_its_keys_only() {
    let mut clip = Clip::from_tracks(
        timebase(),
        30,
        30,
        vec![
            track(X_IN, &[0, 10, 25, 30]),
            track(X_OUT, &[0, 10, 25, 30]),
        ],
    )
    .unwrap();
    clip.stretch_phase(Phase::In, 60).unwrap();
    assert_eq!(frames(&clip, X_IN), [0, 20, 50, 60]);
    assert_eq!(clip.in_frames(), 60);
    assert_eq!(frames(&clip, X_OUT), [0, 10, 25, 30]);
    assert_eq!(clip.out_frames(), 30);
    // Values and easing travel with their keys.
    assert_eq!(clip.key(X_IN, 50).unwrap().value, Value::Scalar(25));
}

#[test]
fn shrinking_until_keys_collide_reports_the_shortest_safe_length() {
    let mut clip =
        Clip::from_tracks(timebase(), 30, 0, vec![track(X_IN, &[0, 10, 25, 30])]).unwrap();
    let before = clip.clone();
    assert_eq!(
        clip.stretch_phase(Phase::In, 3),
        Err(ClipError::RetimeCollision {
            track: X_IN,
            min_len: 4
        })
    );
    assert_eq!(clip, before, "refused edits leave the clip unchanged");
    clip.stretch_phase(Phase::In, 4).unwrap();
    assert_eq!(frames(&clip, X_IN), [0, 1, 3, 4]);
    assert_eq!(
        clip.stretch_phase(Phase::In, 36_001),
        Err(ClipError::PhaseTooLong {
            phase: Phase::In,
            frames: 36_001
        })
    );
}

#[test]
fn selection_retimes_about_its_first_key() {
    let mut clip = Clip::from_tracks(
        timebase(),
        60,
        0,
        vec![track(X_IN, &[10, 20, 30, 40]), track(Y_IN, &[20, 40])],
    )
    .unwrap();
    let mut selection = select(X_IN, &[20, 30, 40]);
    selection.insert(Y_IN, 40);
    let anchor = selection.first_frame().unwrap();
    assert_eq!(anchor, 20);
    let retimed = clip.retime_selection(&selection, anchor, 20, 10).unwrap();
    assert_eq!(frames(&clip, X_IN), [10, 20, 25, 30]);
    assert_eq!(frames(&clip, Y_IN), [20, 30]);
    let mut expected = select(X_IN, &[20, 25, 30]);
    expected.insert(Y_IN, 30);
    assert_eq!(retimed, expected);
}

#[test]
fn selection_retimes_about_the_playhead_rounding_half_up() {
    let mut clip =
        Clip::from_tracks(timebase(), 60, 0, vec![track(X_IN, &[10, 20, 25, 40])]).unwrap();
    let selection = select(X_IN, &[10, 20, 25, 40]);
    // Playhead at 30, halve: 10 -> 20, 20 -> 25, 25 -> 30 + round(-2.5) = 28,
    // 40 -> 35.
    clip.apply(AnimationEdit::RetimeSelection {
        selection,
        anchor: 30,
        old_span: 2,
        new_span: 1,
    })
    .unwrap();
    assert_eq!(frames(&clip, X_IN), [20, 25, 28, 35]);
}

#[test]
fn retime_collisions_report_the_shortest_span() {
    let mut clip =
        Clip::from_tracks(timebase(), 60, 0, vec![track(X_IN, &[10, 20, 30, 40])]).unwrap();
    let before = clip.clone();
    let selection = select(X_IN, &[20, 30, 40]);
    assert_eq!(
        clip.retime_selection(&selection, 20, 20, 1),
        Err(ClipError::RetimeCollision {
            track: X_IN,
            min_len: 2
        })
    );
    assert_eq!(clip, before);
    clip.retime_selection(&selection, 20, 20, 2).unwrap();
    assert_eq!(frames(&clip, X_IN), [10, 20, 21, 22]);
}

#[test]
fn growing_retimes_clamp_into_the_phase() {
    let mut clip = Clip::from_tracks(timebase(), 40, 0, vec![track(X_IN, &[0, 30, 35])]).unwrap();
    // 30 and 35 both clamp to 40 when tripled about 0.
    assert_eq!(
        clip.retime_selection(&select(X_IN, &[30, 35]), 0, 1, 3),
        Err(ClipError::RetimeCollision {
            track: X_IN,
            min_len: 1
        })
    );
    clip.retime_selection(&select(X_IN, &[30]), 0, 3, 4)
        .unwrap();
    assert_eq!(frames(&clip, X_IN), [0, 35, 40]);
    assert_eq!(
        clip.retime_selection(&select(X_IN, &[35]), 0, 0, 1),
        Err(ClipError::ZeroSpan)
    );
}
