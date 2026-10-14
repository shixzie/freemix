use fm_animation::{
    AnimationEdit, Clip, ClipError, Easing, KeySelection, Keyframe, MAX_CLIP_KEYS, MAX_CLIP_TRACKS,
    MAX_PHASE_FRAMES, MAX_TRACK_KEYS, Phase, Property, Rgba, Snap, SnapKind, SnapTargets, Timebase,
    Track, TrackKey, Value,
};
use fm_types::FrameRate;

const A: TrackKey<u32> = TrackKey::new(1, Phase::In, Property::PositionX);
const B: TrackKey<u32> = TrackKey::new(2, Phase::In, Property::Opacity);

fn timebase(numerator: u32, denominator: u32) -> Timebase {
    Timebase::new(FrameRate::new(numerator, denominator).unwrap())
}

fn key(frame: u32, value: i64) -> Keyframe {
    Keyframe::new(frame, Value::Scalar(value), Easing::Linear)
}

fn frames(clip: &Clip<u32>, track: TrackKey<u32>) -> Vec<u32> {
    clip.track(track)
        .map(|track| track.keys.iter().map(|key| key.frame).collect())
        .unwrap_or_default()
}

fn clip() -> Clip<u32> {
    Clip::from_tracks(
        timebase(25, 1),
        50,
        50,
        vec![
            Track::new(A, vec![key(10, 1), key(14, 2), key(30, 3)]),
            Track::new(B, vec![key(12, 100), key(40, 255)]),
        ],
    )
    .unwrap()
}

#[test]
fn snap_picks_the_nearest_candidate_within_the_threshold() {
    let targets = SnapTargets::new()
        .with_playhead(50)
        .with_keys([47, 61])
        .with_phase_bounds(100)
        .with_whole_seconds(timebase(25, 1), 100);
    // 50 is both the playhead and a whole second: the playhead wins.
    assert_eq!(
        targets.snap(52, 2),
        Some(Snap {
            frame: 50,
            kind: SnapKind::Playhead
        })
    );
    assert_eq!(targets.snap(53, 2), None);
    assert_eq!(targets.snap_frame(53, 2), 53);
    assert_eq!(targets.snap_frame(53, 3), 50);
    assert_eq!(
        targets.snap(48, 1),
        Some(Snap {
            frame: 47,
            kind: SnapKind::Key
        })
    );
    assert_eq!(
        targets.snap(99, 4),
        Some(Snap {
            frame: 100,
            kind: SnapKind::PhaseBound
        })
    );
    assert_eq!(
        targets.snap(73, 2),
        Some(Snap {
            frame: 75,
            kind: SnapKind::WholeSecond
        })
    );
    // Equal distance: the earlier frame wins.
    let tie = SnapTargets::new().with_keys([46, 50]);
    assert_eq!(tie.snap_frame(48, 2), 46);
    assert_eq!(tie.snap(48, 1), None);
}

#[test]
fn whole_seconds_at_ntsc_rates_are_the_first_frame_at_or_after_each_second() {
    let targets = SnapTargets::new().with_whole_seconds(timebase(30_000, 1_001), 1_100);
    // 34 s is 1018.98 frames: snaps to 1019, never 1020.
    assert_eq!(targets.snap_frame(1_020, 0), 1_020);
    assert_eq!(targets.snap_frame(1_020, 1), 1_019);
    assert_eq!(targets.snap_frame(29, 1), 30);
}

#[test]
fn copy_and_paste_keep_relative_offsets() {
    let mut clip = clip();
    let mut selection = KeySelection::new();
    selection.insert(A, 10);
    selection.insert(A, 14);
    selection.insert(B, 12);
    let clipboard = clip.copy_keys(&selection).unwrap();
    let mut offsets: Vec<(u32, Property)> = clipboard
        .keys
        .iter()
        .map(|entry| (entry.offset, entry.property))
        .collect();
    offsets.sort();
    assert_eq!(
        offsets,
        [
            (0, Property::PositionX),
            (2, Property::Opacity),
            (4, Property::PositionX)
        ]
    );
    let pasted = clip.paste(&clipboard, Phase::Out, 5).unwrap();
    let a_out = TrackKey::new(1, Phase::Out, Property::PositionX);
    let b_out = TrackKey::new(2, Phase::Out, Property::Opacity);
    assert_eq!(frames(&clip, a_out), [5, 9]);
    assert_eq!(frames(&clip, b_out), [7]);
    assert_eq!(pasted.len(), 3);
    assert!(pasted.contains(b_out, 7));
    assert_eq!(clip.key(a_out, 9).unwrap().value, Value::Scalar(2));
    // Pasting onto existing keys replaces them.
    clip.paste(&clipboard, Phase::In, 30).unwrap();
    assert_eq!(frames(&clip, A), [10, 14, 30, 34]);
    assert_eq!(clip.key(A, 30).unwrap().value, Value::Scalar(1));
    // Past the phase end is refused atomically.
    let before = clip.clone();
    assert_eq!(
        clip.paste(&clipboard, Phase::In, 48),
        Err(ClipError::KeyOutsidePhase {
            track: A,
            frame: 52
        })
    );
    assert_eq!(clip, before);
}

#[test]
fn easing_is_set_on_the_selection() {
    let mut clip = clip();
    let mut selection = KeySelection::new();
    selection.insert(A, 14);
    selection.insert(B, 12);
    let kept = clip
        .apply(AnimationEdit::SetEasing {
            selection: selection.clone(),
            easing: Easing::BounceOut,
        })
        .unwrap();
    assert_eq!(kept, selection);
    assert_eq!(clip.key(A, 14).unwrap().easing, Easing::BounceOut);
    assert_eq!(clip.key(B, 12).unwrap().easing, Easing::BounceOut);
    assert_eq!(clip.key(A, 10).unwrap().easing, Easing::Linear);
    let mut missing = KeySelection::new();
    missing.insert(A, 11);
    assert_eq!(
        clip.set_easing(&missing, Easing::Hold),
        Err(ClipError::UnknownKey {
            track: A,
            frame: 11
        })
    );
}

#[test]
fn moves_clamp_to_the_phase_and_refuse_collisions() {
    let mut clip = clip();
    let mut selection = KeySelection::new();
    selection.insert(A, 10);
    selection.insert(A, 14);
    let moved = clip.move_keys(&selection, -100).unwrap();
    assert_eq!(frames(&clip, A), [0, 4, 30]);
    assert!(moved.contains(A, 0) && moved.contains(A, 4));
    let moved = clip.move_keys(&moved, 1_000).unwrap();
    // Clamped so the later key lands on the phase end (50): 46 and 50, which
    // crosses the unselected key at 30.
    assert_eq!(frames(&clip, A), [30, 46, 50]);
    let before = clip.clone();
    let mut one = KeySelection::new();
    one.insert(A, 46);
    assert_eq!(
        clip.move_keys(&one, -16),
        Err(ClipError::KeyCollision {
            track: A,
            frame: 30
        })
    );
    assert_eq!(clip, before);
    assert_eq!(moved.len(), 2);
}

#[test]
fn set_key_and_remove_keys_manage_tracks() {
    let mut clip = clip();
    let color = TrackKey::new(3, Phase::Out, Property::Color);
    clip.set_key(
        color,
        Keyframe::new(0, Value::Color(Rgba::new(1, 2, 3, 4)), Easing::Linear),
    )
    .unwrap();
    assert_eq!(
        clip.set_key(color, key(5, 3)),
        Err(ClipError::InvalidValue {
            track: color,
            frame: 5
        })
    );
    let mut selection = KeySelection::new();
    selection.insert(color, 0);
    selection.insert(B, 12);
    clip.apply(AnimationEdit::RemoveKeys(selection)).unwrap();
    assert!(clip.track(color).is_none(), "emptied tracks are removed");
    assert_eq!(frames(&clip, B), [40]);
}

#[test]
fn limits_are_validated() {
    let tb = timebase(25, 1);
    assert_eq!(
        Clip::<u32>::new(tb, MAX_PHASE_FRAMES + 1, 0),
        Err(ClipError::PhaseTooLong {
            phase: Phase::In,
            frames: MAX_PHASE_FRAMES + 1
        })
    );
    let tracks: Vec<Track<u32>> = (0..=u32::try_from(MAX_CLIP_TRACKS).unwrap())
        .map(|target| {
            Track::new(
                TrackKey::new(target, Phase::In, Property::PositionX),
                vec![key(0, 0)],
            )
        })
        .collect();
    assert_eq!(
        Clip::from_tracks(tb, 10, 10, tracks),
        Err(ClipError::TooManyTracks {
            count: MAX_CLIP_TRACKS + 1
        })
    );
    let long: Vec<Keyframe> = (0..=u32::try_from(MAX_TRACK_KEYS).unwrap())
        .map(|frame| key(frame, 0))
        .collect();
    assert_eq!(
        Clip::from_tracks(tb, 1_000, 0, vec![Track::new(A, long)]),
        Err(ClipError::TooManyTrackKeys {
            track: A,
            count: MAX_TRACK_KEYS + 1
        })
    );
    // 17 tracks of 241 keys is 4097 keys.
    let per_track = 241;
    assert!(17 * per_track > MAX_CLIP_KEYS);
    let tracks: Vec<Track<u32>> = (0..17)
        .map(|target| {
            Track::new(
                TrackKey::new(target, Phase::In, Property::PositionX),
                (0..u32::try_from(per_track).unwrap())
                    .map(|frame| key(frame, 0))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        Clip::from_tracks(tb, 1_000, 0, tracks),
        Err(ClipError::TooManyKeys { count: 17 * 241 })
    );
    assert_eq!(
        Clip::from_tracks(tb, 10, 0, vec![Track::new(A, vec![key(3, 0), key(3, 1)])]),
        Err(ClipError::NonIncreasingKeys { track: A, frame: 3 })
    );
    assert_eq!(
        Clip::from_tracks(tb, 10, 0, vec![Track::new(A, vec![key(11, 0)])]),
        Err(ClipError::KeyOutsidePhase {
            track: A,
            frame: 11
        })
    );
    assert_eq!(
        Clip::from_tracks(tb, 10, 0, vec![Track::new(B, vec![key(1, 256)])]),
        Err(ClipError::InvalidValue { track: B, frame: 1 })
    );
    assert_eq!(
        Clip::from_tracks(tb, 10, 0, vec![Track::new(A, Vec::new())]),
        Err(ClipError::EmptyTrack(A))
    );
    assert_eq!(
        Clip::from_tracks(
            tb,
            10,
            0,
            vec![
                Track::new(A, vec![key(1, 0)]),
                Track::new(A, vec![key(2, 0)])
            ]
        ),
        Err(ClipError::DuplicateTrack(A))
    );
    let mut clip = clip();
    assert_eq!(
        clip.set_phase_frames_keeping_keys(Phase::In, 20),
        Err(ClipError::KeyOutsidePhase {
            track: A,
            frame: 30
        })
    );
}
