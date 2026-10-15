use core::time::Duration;

use fm_animation::{
    AnimationPreset, Clip, Easing, KeyRef, Keyframe, Phase, Property, SnapKind, Timebase, Track,
    TrackKey, Value,
};
use fm_types::FrameRate;

use super::*;

const TARGET: u32 = 1;

fn clip(in_frames: u32, tracks: &[(Property, &[(u32, i64)])]) -> Clip<u32> {
    let tracks = tracks
        .iter()
        .map(|(property, keys)| {
            Track::new(
                TrackKey::new(TARGET, Phase::In, *property),
                keys.iter()
                    .map(|&(frame, value)| {
                        Keyframe::new(frame, Value::Scalar(value), Easing::Linear)
                    })
                    .collect(),
            )
        })
        .collect();
    Clip::from_tracks(
        Timebase::new(FrameRate::new(50, 1).unwrap()),
        in_frames,
        25,
        tracks,
    )
    .unwrap()
}

fn document(in_frames: u32, tracks: &[(Property, &[(u32, i64)])]) -> AnimDocument<u32> {
    let mut document = AnimDocument::new(clip(in_frames, tracks), 7, 1_920, 1_080);
    document.set_targets([TargetOutline::new(
        TARGET,
        "Lower third",
        TargetKind::Shape,
        TargetBounds::new(100, 50, 200, 80),
    )]);
    document
}

fn track(property: Property) -> TrackKey<u32> {
    TrackKey::new(TARGET, Phase::In, property)
}

fn key(property: Property, frame: u32) -> KeyRef<u32> {
    KeyRef {
        track: track(property),
        frame,
    }
}

fn frames(document: &AnimDocument<u32>, property: Property) -> Vec<u32> {
    document
        .clip()
        .track(track(property))
        .map(|track| track.keys.iter().map(|key| key.frame).collect())
        .unwrap_or_default()
}

fn scalar(document: &AnimDocument<u32>, property: Property, frame: u32) -> i64 {
    match document
        .clip()
        .key(track(property), frame)
        .map(|key| key.value)
    {
        Some(Value::Scalar(value)) => value,
        other => panic!("{property:?}@{frame}: {other:?}"),
    }
}

fn approx(left: f32, right: f32) -> bool {
    (left - right).abs() < 1e-3
}

const POSITION: &[(u32, i64)] = &[(0, 0), (20, 100), (40, 200)];

#[test]
fn key_drag_snaps_to_the_playhead_and_commits_one_undo_step() {
    let mut document = document(50, &[(Property::PositionX, POSITION)]);
    document.set_playhead(30);
    assert_eq!(document.view().snap_threshold_frames(), 2);
    let hit = key(Property::PositionX, 20);
    document
        .begin_drag(DragStart::Key(hit), Pointer::at(20), Modifiers::NONE)
        .unwrap();
    let generation = document.generation();
    for frame in [22, 25, 27] {
        assert_eq!(
            document
                .update_drag(Pointer::at(frame), Modifiers::NONE)
                .unwrap(),
            None
        );
    }
    assert_eq!(frames(&document, Property::PositionX), [0, 27, 40]);
    let snap = document
        .update_drag(Pointer::at(29), Modifiers::NONE)
        .unwrap()
        .unwrap();
    assert_eq!((snap.frame, snap.kind), (30, SnapKind::Playhead));
    assert!(document.generation() > generation);
    let outcome = document.commit_drag().unwrap();
    assert!(outcome.edited && outcome.rejected.is_none());
    assert_eq!(frames(&document, Property::PositionX), [0, 30, 40]);
    assert_eq!(scalar(&document, Property::PositionX, 30), 100);
    assert!(
        document
            .selection()
            .contains(track(Property::PositionX), 30)
    );
    assert_eq!(document.undo_depth(), 1);
    assert!(document.is_dirty());

    document.undo().unwrap();
    assert_eq!(frames(&document, Property::PositionX), [0, 20, 40]);
    assert!(
        document
            .selection()
            .contains(track(Property::PositionX), 20)
    );
    assert_eq!(document.undo_depth(), 0);
}

#[test]
fn key_drag_with_shift_ignores_snapping_and_is_one_undo_step() {
    let mut document = document(50, &[(Property::PositionX, POSITION)]);
    document.set_playhead(30);
    document
        .begin_drag(
            DragStart::Key(key(Property::PositionX, 20)),
            Pointer::at(20),
            Modifiers::SHIFT,
        )
        .unwrap();
    for frame in [24, 28, 29] {
        assert_eq!(
            document
                .update_drag(Pointer::at(frame), Modifiers::SHIFT)
                .unwrap(),
            None
        );
    }
    assert!(document.commit_drag().unwrap().edited);
    assert_eq!(frames(&document, Property::PositionX), [0, 29, 40]);
    assert_eq!(document.undo_depth(), 1);
    assert!(!document.can_redo());
}

#[test]
fn cancelled_drag_restores_draft_and_selection() {
    let mut document = document(50, &[(Property::PositionX, POSITION)]);
    let generation = document.generation();
    document
        .begin_drag(
            DragStart::Key(key(Property::PositionX, 20)),
            Pointer::at(20),
            Modifiers::NONE,
        )
        .unwrap();
    document
        .update_drag(Pointer::at(33), Modifiers::SHIFT)
        .unwrap();
    assert_eq!(
        document.undo(),
        Err(AnimEditorError::DragActive),
        "no undo mid-drag"
    );
    document.cancel_drag().unwrap();
    assert_eq!(frames(&document, Property::PositionX), [0, 20, 40]);
    let selected: Vec<_> = document.selection().iter().collect();
    assert_eq!(
        selected,
        [key(Property::PositionX, 20)],
        "the press selection stays"
    );
    assert_eq!(document.undo_depth(), 0);
    assert!(!document.is_dirty());
    assert!(document.generation() > generation);
    assert_eq!(document.cancel_drag(), Err(AnimEditorError::NoDrag));
}

#[test]
fn drag_onto_another_key_reports_collision_and_keeps_last_valid_state() {
    let mut document = document(50, &[(Property::PositionX, POSITION)]);
    document
        .begin_drag(
            DragStart::Key(key(Property::PositionX, 20)),
            Pointer::at(20),
            Modifiers::SHIFT,
        )
        .unwrap();
    document
        .update_drag(Pointer::at(35), Modifiers::SHIFT)
        .unwrap();
    let error = document
        .update_drag(Pointer::at(40), Modifiers::SHIFT)
        .unwrap_err();
    assert!(matches!(
        error,
        AnimEditorError::KeyCollision { frame: 40, .. }
    ));
    assert_eq!(error.msg_key(), "anim-editor-key-collision");
    assert_eq!(frames(&document, Property::PositionX), [0, 35, 40]);
    assert_eq!(document.drag().unwrap().rejected(), Some(&error));
    let outcome = document.commit_drag().unwrap();
    assert!(outcome.edited);
    assert_eq!(outcome.rejected, Some(error));
    assert_eq!(document.undo_depth(), 1);
}

#[test]
fn box_select_replaces_adds_and_toggles() {
    let mut document = document(
        50,
        &[
            (Property::PositionX, POSITION),
            (Property::Opacity, &[(10, 0), (30, 255)]),
        ],
    );
    assert_eq!(
        document.rows(),
        [track(Property::PositionX), track(Property::Opacity)]
    );
    document
        .begin_drag(DragStart::SelectionBox, Pointer::new(5, 0), Modifiers::NONE)
        .unwrap();
    document
        .update_drag(Pointer::new(25, 1), Modifiers::NONE)
        .unwrap();
    assert_eq!(
        document.drag().unwrap().box_range(),
        Some(((5, 25), (0, 1)))
    );
    let outcome = document.commit_drag().unwrap();
    assert!(!outcome.edited);
    let selected: Vec<_> = document.selection().iter().collect();
    assert_eq!(
        selected,
        [key(Property::PositionX, 20), key(Property::Opacity, 10)]
    );

    // Ctrl box over row 0 frames 15..=45 toggles 20 off and 40 on.
    document
        .begin_drag(
            DragStart::SelectionBox,
            Pointer::new(15, 0),
            Modifiers::CTRL,
        )
        .unwrap();
    document
        .update_drag(Pointer::new(45, 0), Modifiers::CTRL)
        .unwrap();
    document.commit_drag().unwrap();
    let selected: Vec<_> = document.selection().iter().collect();
    assert_eq!(
        selected,
        [key(Property::PositionX, 40), key(Property::Opacity, 10)]
    );
    assert_eq!(document.undo_depth(), 0);
}

#[test]
fn click_selection_plain_ctrl_and_shift() {
    let mut document = document(
        50,
        &[(Property::PositionX, &[(0, 0), (10, 1), (20, 2), (30, 3)])],
    );
    document
        .click_key(key(Property::PositionX, 10), Modifiers::NONE)
        .unwrap();
    document
        .click_key(key(Property::PositionX, 30), Modifiers::SHIFT)
        .unwrap();
    assert_eq!(document.selection().len(), 3);
    document
        .click_key(key(Property::PositionX, 20), Modifiers::CTRL)
        .unwrap();
    assert_eq!(document.selection().len(), 2);
    assert!(
        !document
            .selection()
            .contains(track(Property::PositionX), 20)
    );
    document.click_empty(Modifiers::NONE);
    assert!(document.selection().is_empty());
    assert!(matches!(
        document.click_key(key(Property::PositionX, 5), Modifiers::NONE),
        Err(AnimEditorError::UnknownKey { frame: 5, .. })
    ));
}

const RETIME: &[(u32, i64)] = &[(10, 0), (20, 50), (30, 100)];

fn select_all_and_drag(
    document: &mut AnimDocument<u32>,
    handle: RetimeHandle,
    from: u32,
    to: u32,
    modifiers: Modifiers,
) -> Result<Option<fm_animation::Snap>, AnimEditorError<u32>> {
    document.select_all();
    document
        .begin_drag(DragStart::Retime(handle), Pointer::at(from), modifiers)
        .unwrap();
    let result = document.update_drag(Pointer::at(to), modifiers);
    document.commit_drag().unwrap();
    result
}

#[test]
fn retime_handles_stretch_proportionally() {
    let mut document = document(100, &[(Property::PositionX, RETIME)]);
    select_all_and_drag(
        &mut document,
        RetimeHandle::SelectionEnd,
        30,
        50,
        Modifiers::SHIFT,
    )
    .unwrap();
    assert_eq!(frames(&document, Property::PositionX), [10, 30, 50]);
    assert_eq!(document.selection().len(), 3);
    assert_eq!(document.undo_depth(), 1);

    document.undo().unwrap();
    select_all_and_drag(
        &mut document,
        RetimeHandle::SelectionStart,
        10,
        0,
        Modifiers::SHIFT,
    )
    .unwrap();
    assert_eq!(frames(&document, Property::PositionX), [0, 15, 30]);
    assert_eq!(document.undo_depth(), 1);
}

#[test]
fn retime_handle_with_alt_extends_without_scaling() {
    let mut document = document(100, &[(Property::PositionX, RETIME)]);
    let alt_shift = Modifiers {
        alt: true,
        shift: true,
        ..Modifiers::NONE
    };
    select_all_and_drag(&mut document, RetimeHandle::SelectionEnd, 30, 50, alt_shift).unwrap();
    assert_eq!(frames(&document, Property::PositionX), [10, 20, 50]);
    assert_eq!(scalar(&document, Property::PositionX, 50), 100);
    let selected: Vec<u32> = document.selection().iter().map(|key| key.frame).collect();
    assert_eq!(selected, [10, 20, 50]);

    // Phase end: alt lengthens the phase, keys stay; without alt they stretch.
    select_all_and_drag(&mut document, RetimeHandle::PhaseEnd, 100, 150, alt_shift).unwrap();
    assert_eq!(document.phase_frames(), 150);
    assert_eq!(frames(&document, Property::PositionX), [10, 20, 50]);
    select_all_and_drag(
        &mut document,
        RetimeHandle::PhaseEnd,
        150,
        300,
        Modifiers::SHIFT,
    )
    .unwrap();
    assert_eq!(document.phase_frames(), 300);
    assert_eq!(frames(&document, Property::PositionX), [20, 40, 100]);
    assert_eq!(document.undo_depth(), 3);
}

#[test]
fn retime_collision_reports_min_len() {
    let mut document = document(100, &[(Property::PositionX, &[(10, 0), (11, 1), (12, 2)])]);
    let error = select_all_and_drag(
        &mut document,
        RetimeHandle::SelectionEnd,
        12,
        11,
        Modifiers::SHIFT,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        AnimEditorError::RetimeCollision { min_len: 2, .. }
    ));
    assert_eq!(error.msg_key(), "anim-editor-retime-collision");
    assert_eq!(frames(&document, Property::PositionX), [10, 11, 12]);
    assert_eq!(document.undo_depth(), 0, "a refused drag records nothing");

    // Phase stretch collisions name the shortest length that fits.
    let mut document = document_with_dense_keys();
    let error = document.set_phase_duration(Phase::In, 2).unwrap_err();
    let AnimEditorError::RetimeCollision { min_len, .. } = error else {
        panic!("{error:?}");
    };
    assert!(min_len > 2);
    assert_eq!(document.phase_frames(), 10);
    document.set_phase_duration(Phase::In, min_len).unwrap();
    assert_eq!(document.phase_frames(), min_len);
}

fn document_with_dense_keys() -> AnimDocument<u32> {
    document(
        10,
        &[(Property::PositionX, &[(0, 0), (1, 1), (2, 2), (10, 3)])],
    )
}

#[test]
fn key_at_playhead_inserts_the_sampled_value() {
    let mut document = document(50, &[(Property::PositionX, POSITION)]);
    document.set_playhead(10);
    let inserted = document
        .key_at_playhead(TARGET, Property::PositionX)
        .unwrap();
    assert_eq!(inserted, key(Property::PositionX, 10));
    assert_eq!(frames(&document, Property::PositionX), [0, 10, 20, 40]);
    assert_eq!(scalar(&document, Property::PositionX, 10), 50);
    assert!(
        document
            .selection()
            .contains(track(Property::PositionX), 10)
    );

    // An unanimated property keys its identity.
    document.key_at_playhead(TARGET, Property::Opacity).unwrap();
    assert_eq!(scalar(&document, Property::Opacity, 10), 255);
    assert_eq!(document.undo_depth(), 2);

    // Keying an existing key changes nothing; setting a value updates it.
    document
        .key_at_playhead(TARGET, Property::PositionX)
        .unwrap();
    assert_eq!(document.undo_depth(), 2);
    document
        .set_value_at_playhead(TARGET, Property::PositionX, Value::Scalar(75))
        .unwrap();
    assert_eq!(scalar(&document, Property::PositionX, 10), 75);
    assert_eq!(document.undo_depth(), 3);

    assert_eq!(
        document.key_at_playhead(9, Property::PositionX),
        Err(AnimEditorError::UnknownTarget(9))
    );
    assert!(matches!(
        document.set_value_at_playhead(TARGET, Property::Opacity, Value::Scalar(999)),
        Err(AnimEditorError::InvalidValue { .. })
    ));
}

#[test]
fn copy_paste_at_playhead() {
    let mut document = document(50, &[(Property::PositionX, POSITION)]);
    assert_eq!(
        document.paste_at_playhead(),
        Err(AnimEditorError::EmptyClipboard)
    );
    assert_eq!(
        document.copy_selection(),
        Err(AnimEditorError::EmptySelection)
    );
    document
        .click_key(key(Property::PositionX, 0), Modifiers::NONE)
        .unwrap();
    document
        .click_key(key(Property::PositionX, 20), Modifiers::CTRL)
        .unwrap();
    assert_eq!(document.copy_selection().unwrap(), 2);
    document.set_playhead(25);
    document.paste_at_playhead().unwrap();
    assert_eq!(frames(&document, Property::PositionX), [0, 20, 25, 40, 45]);
    assert_eq!(scalar(&document, Property::PositionX, 25), 0);
    assert_eq!(scalar(&document, Property::PositionX, 45), 100);
    let selected: Vec<u32> = document.selection().iter().map(|key| key.frame).collect();
    assert_eq!(selected, [25, 45]);
    assert_eq!(document.undo_depth(), 1);

    // Past the phase end: refused, nothing changes.
    document.set_playhead(40);
    assert!(matches!(
        document.paste_at_playhead(),
        Err(AnimEditorError::KeyOutsidePhase { frame: 60, .. })
    ));
    assert_eq!(frames(&document, Property::PositionX), [0, 20, 25, 40, 45]);
    assert_eq!(document.undo_depth(), 1);
}

#[test]
fn preset_apply_then_undo_and_redo() {
    let mut document = document(10, &[]);
    document
        .apply_preset(TARGET, Phase::In, AnimationPreset::Pop, 20)
        .unwrap();
    assert_eq!(document.phase_frames(), 20, "phase grows to fit");
    assert_eq!(frames(&document, Property::ScaleX), [0, 20]);
    assert_eq!(frames(&document, Property::ScaleY), [0, 20]);
    assert_eq!(document.selection().len(), 4);
    assert_eq!(document.undo_depth(), 1);
    let applied = document.clip().clone();

    document.undo().unwrap();
    assert_eq!(document.clip().tracks().len(), 0);
    assert_eq!(document.phase_frames(), 10);
    document.redo().unwrap();
    assert_eq!(document.clip(), &applied);
    assert_eq!(document.redo(), Err(AnimEditorError::NothingToRedo));

    assert_eq!(
        document.apply_preset(2, Phase::In, AnimationPreset::Fade, 10),
        Err(AnimEditorError::UnknownTarget(2))
    );
}

#[test]
fn phase_duration_stretches_keys() {
    let mut document = document(50, &[(Property::PositionX, &[(0, 0), (25, 1), (50, 2)])]);
    document.select_all();
    document.set_phase_duration(Phase::In, 100).unwrap();
    assert_eq!(frames(&document, Property::PositionX), [0, 50, 100]);
    let selected: Vec<u32> = document.selection().iter().map(|key| key.frame).collect();
    assert_eq!(selected, [0, 50, 100]);
    assert_eq!(document.undo_depth(), 1);
    assert!(matches!(
        document.set_phase_duration(Phase::In, 40_000),
        Err(AnimEditorError::LimitExceeded(
            AnimLimit::PhaseFrames { .. }
        ))
    ));
    assert!(matches!(
        document.extend_phase(Phase::In, 60),
        Err(AnimEditorError::KeyOutsidePhase { frame: 100, .. })
    ));
}

#[test]
fn nudge_delete_easing_each_one_undo_step() {
    let mut document = document(50, &[(Property::PositionX, POSITION)]);
    document
        .click_key(key(Property::PositionX, 20), Modifiers::NONE)
        .unwrap();
    document.nudge_selection(1).unwrap();
    document.nudge_selection(10).unwrap();
    assert_eq!(frames(&document, Property::PositionX), [0, 31, 40]);
    assert!(matches!(
        document.nudge_selection(9),
        Err(AnimEditorError::KeyCollision { frame: 40, .. })
    ));
    document.nudge_selection(-10).unwrap();
    document.set_easing(Easing::CubicOut).unwrap();
    assert_eq!(
        document
            .clip()
            .key(track(Property::PositionX), 21)
            .unwrap()
            .easing,
        Easing::CubicOut
    );
    document.delete_selection().unwrap();
    assert_eq!(frames(&document, Property::PositionX), [0, 40]);
    assert_eq!(document.undo_depth(), 5);
    for _ in 0..5 {
        document.undo().unwrap();
    }
    assert_eq!(frames(&document, Property::PositionX), [0, 20, 40]);
    assert_eq!(document.undo(), Err(AnimEditorError::NothingToUndo));
}

#[test]
fn undo_history_is_bounded() {
    let mut document = document(50, &[(Property::PositionX, POSITION)]);
    for value in 0..i64::try_from(UNDO_LIMIT + 20).unwrap() {
        document
            .set_value_at_playhead(TARGET, Property::PositionX, Value::Scalar(value))
            .unwrap();
    }
    assert_eq!(document.undo_depth(), UNDO_LIMIT);
    while document.undo().is_ok() {}
    assert_eq!(scalar(&document, Property::PositionX, 0), 19);
    assert_eq!(document.redo_depth(), UNDO_LIMIT);
}

#[test]
fn authority_reset_and_save_track_revision() {
    let mut document = document(50, &[(Property::PositionX, POSITION)]);
    document.select_all();
    document.nudge_selection(1).unwrap();
    assert!(document.is_dirty());
    document.mark_saved(8);
    assert!(!document.is_dirty());
    assert_eq!(document.base_revision(), 8);
    assert!(document.can_undo());
    document.reset_from_authority(clip(50, &[(Property::PositionX, &[(1, 0)])]), 9);
    assert_eq!(document.base_revision(), 9);
    assert!(!document.can_undo());
    let selected: Vec<_> = document.selection().iter().collect();
    assert_eq!(selected, [key(Property::PositionX, 1)]);
}

#[test]
fn playback_follows_clock_ticks() {
    let mut document = document(50, &[]);
    document.play();
    assert!(document.tick(Duration::from_millis(100)));
    assert_eq!(document.playhead(), 5);
    assert!(document.tick(Duration::from_millis(10)));
    assert_eq!(document.playhead(), 5);
    assert!(document.tick(Duration::from_millis(10)));
    assert_eq!(document.playhead(), 6);
    assert!(!document.tick(Duration::from_secs(5)));
    assert_eq!(document.playhead(), 50);
    document.step(-10);
    assert_eq!(document.playhead(), 40);
    document.set_looping(true);
    document.play();
    assert!(document.tick(Duration::from_millis(240)));
    assert_eq!(document.playhead(), 1, "40 + 12 wraps over 0..=50");
}

fn preset_document(preset: AnimationPreset) -> AnimDocument<u32> {
    let mut document = document(20, &[]);
    document
        .apply_preset(TARGET, Phase::In, preset, 20)
        .unwrap();
    document.set_playhead(10);
    document
}

#[test]
fn proxy_quads_mid_slide() {
    let document = preset_document(AnimationPreset::SlideFromLeft);
    let sample = &document.sample_at_playhead()[0];
    let Some(Value::Scalar(offset)) = sample.get(Property::PositionX) else {
        panic!("{sample:?}");
    };
    assert!((-300..0).contains(&offset), "{offset}");
    let quads = document.proxy_at_playhead();
    let [quad] = quads.as_slice() else {
        panic!("{quads:?}");
    };
    let expected_left = 100.0 + f32::from(i16::try_from(offset).unwrap());
    assert_eq!(
        quad.aabb(),
        [expected_left, 50.0, expected_left + 200.0, 130.0]
    );
    assert!(approx(quad.opacity, 1.0));
    // At the end it rests on the static design.
    let end = document.proxy_at(20);
    assert_eq!(end[0].aabb(), [100.0, 50.0, 300.0, 130.0]);
}

#[test]
fn proxy_quads_mid_pop() {
    let document = preset_document(AnimationPreset::Pop);
    let sample = &document.sample_at_playhead()[0];
    let Some(Value::Scalar(scale)) = sample.get(Property::ScaleX) else {
        panic!("{sample:?}");
    };
    assert!(scale > 0 && scale != 10_000, "{scale}");
    let quad = &document.proxy_at_playhead()[0];
    let factor = f32::from(u16::try_from(scale).unwrap()) / 10_000.0;
    let [min_x, min_y, max_x, max_y] = quad.aabb();
    assert!(approx(max_x - min_x, 200.0 * factor));
    assert!(approx(max_y - min_y, 80.0 * factor));
    let [center_x, center_y] = quad.center();
    assert!(approx(center_x, 200.0) && approx(center_y, 90.0));
    let start = document.proxy_at(0);
    assert_eq!(
        start[0].aabb(),
        [200.0, 90.0, 200.0, 90.0],
        "pops from a point"
    );
}

#[test]
fn proxy_quads_mid_fade_and_onion_skins() {
    let document = preset_document(AnimationPreset::Fade);
    let quad = &document.proxy_at_playhead()[0];
    assert!(approx(quad.opacity, 127.0 / 255.0), "{}", quad.opacity);
    assert_eq!(quad.aabb(), [100.0, 50.0, 300.0, 130.0]);
    assert!(approx(document.proxy_at(0)[0].opacity, 0.0));

    let skins = document.onion_skins(3, 4);
    let offsets: Vec<i64> = skins.iter().map(|skin| skin.offset).collect();
    assert_eq!(offsets, [-8, -4, 4, 8], "only frames inside the phase");
    assert!(skins[0].quads[0].opacity < quad.opacity);
    assert!(skins[3].quads[0].opacity > quad.opacity);
}

#[test]
fn proxy_applies_rotation_crop_and_colour() {
    let mut document = document(
        20,
        &[
            (Property::Rotation, &[(0, 90_000)]),
            (Property::CropLeft, &[(0, 50)]),
        ],
    );
    document
        .set_value_at_playhead(
            TARGET,
            Property::Color,
            Value::Color(fm_animation::Rgba::new(255, 0, 0, 255)),
        )
        .unwrap();
    let quad = &document.proxy_at_playhead()[0];
    assert_eq!(quad.color, fm_animation::Rgba::new(255, 0, 0, 255));
    // Cropped to x 150..300 (local -50..100), rotated 90 degrees about
    // (200, 90): top-left (-50, -40) maps to (240, 40).
    assert!(approx(quad.corners[0][0], 240.0) && approx(quad.corners[0][1], 40.0));
    let [min_x, min_y, max_x, max_y] = quad.aabb();
    assert!(approx(max_x - min_x, 80.0) && approx(max_y - min_y, 150.0));
}

fn preview_key(generation: u64, frame: u32) -> PreviewKey {
    PreviewKey {
        generation,
        phase: Phase::In,
        frame,
    }
}

#[test]
fn preview_scheduler_coalesces_latest_wins() {
    let ms = Duration::from_millis;
    let mut scheduler = PreviewScheduler::new(ms(100));
    scheduler.request(preview_key(1, 1), ms(0));
    scheduler.request(preview_key(1, 2), ms(30));
    scheduler.request(preview_key(2, 3), ms(60));
    assert_eq!(scheduler.poll(ms(150)), None, "debounce restarts on change");
    assert_eq!(scheduler.next_deadline(), Some(ms(160)));
    let first = scheduler.poll(ms(160)).unwrap();
    assert_eq!(first.key, preview_key(2, 3));

    // While in flight, requests coalesce into one pending key.
    scheduler.request(preview_key(3, 4), ms(170));
    scheduler.request(preview_key(3, 5), ms(180));
    assert_eq!(scheduler.poll(ms(400)), None, "one request in flight");
    assert_eq!(scheduler.next_deadline(), None);
    assert_eq!(scheduler.complete(first.id), PreviewCompletion::Superseded);
    assert_eq!(scheduler.complete(first.id), PreviewCompletion::Unknown);
    let second = scheduler.poll(ms(400)).unwrap();
    assert_eq!(second.key, preview_key(3, 5));
    assert!(second.id > first.id);

    // Requesting what is in flight or delivered needs nothing new.
    scheduler.request(preview_key(3, 5), ms(410));
    assert_eq!(scheduler.pending(), None);
    assert_eq!(scheduler.complete(second.id), PreviewCompletion::Current);
    scheduler.request(preview_key(3, 5), ms(500));
    assert_eq!(scheduler.poll(ms(1_000)), None);

    // A failure frees the slot; a reset forgets late results.
    scheduler.request(preview_key(4, 5), ms(1_000));
    let third = scheduler.poll(ms(1_100)).unwrap();
    assert!(scheduler.fail(third.id));
    scheduler.request(preview_key(4, 6), ms(1_100));
    let fourth = scheduler.poll(ms(1_200)).unwrap();
    scheduler.reset();
    assert_eq!(scheduler.complete(fourth.id), PreviewCompletion::Unknown);
}

#[test]
fn document_preview_key_tracks_generation() {
    let mut document = document(50, &[(Property::PositionX, POSITION)]);
    let before = document.preview_key();
    document.set_playhead(5);
    let moved = document.preview_key();
    assert_eq!((moved.generation, moved.frame), (before.generation, 5));
    document
        .key_at_playhead(TARGET, Property::PositionX)
        .unwrap();
    assert!(document.preview_key().generation > before.generation);
}

#[test]
fn timeline_view_maps_frames_and_pixels() {
    let mut view = TimelineView::default();
    assert_eq!(view.frame_at_x(40.0), 10);
    assert!(approx(view.x_of_frame(10), 40.0));
    view.scroll_to(100.0);
    view.zoom_about(2.0, 40.0);
    assert!(approx(view.frames_per_pixel(), 0.5));
    assert_eq!(view.frame_at_x(40.0), 110, "anchor frame stays put");
    view.set_frames_per_pixel(1_000.0);
    assert!(approx(view.frames_per_pixel(), MAX_FRAMES_PER_PIXEL));
    view.scroll_to(-5.0);
    assert!(approx(view.scroll_frame(), 0.0));
    assert_eq!(view.visible_range(10.0), (0, 640));
}

// ----- custom curves ------------------------------------------------------

fn bezier(document: &AnimDocument<u32>, property: Property, frame: u32) -> [f64; 4] {
    match document
        .clip()
        .key(track(property), frame)
        .map(|key| key.easing)
    {
        Some(Easing::CubicBezier(curve)) => curve::unit_controls(curve),
        other => panic!("{property:?}@{frame} is not a custom curve: {other:?}"),
    }
}

fn close(left: [f64; 4], right: [f64; 4]) -> bool {
    left.iter()
        .zip(right)
        .all(|(left, right)| (left - right).abs() < 1e-4)
}

fn easing(document: &AnimDocument<u32>, property: Property, frame: u32) -> Easing {
    document.clip().key(track(property), frame).unwrap().easing
}

#[test]
fn curve_points_are_constrained_to_the_engines_limits() {
    use fm_animation::{EASE_ONE, MAX_BEZIER_Y, MIN_BEZIER_Y};
    assert_eq!(curve::constrain_point([-0.4, 3.5]), (0, MAX_BEZIER_Y));
    assert_eq!(
        curve::constrain_point([1.7, -9.0]),
        (EASE_ONE, MIN_BEZIER_Y)
    );
    assert_eq!(curve::constrain_point([0.5, 1.25]), (32_768, 81_920));
    assert_eq!(curve::constrain_point([f64::NAN, f64::INFINITY]), (0, 0));
    let start = CurvePreset::EaseInOut.curve();
    let (moved, _) = curve::with_handle(start, CurveHandle::In, [4.0, -4.0], None);
    assert!(close(
        curve::unit_controls(moved),
        [0.42, 0.0, 1.0, MIN_CURVE_Y]
    ));
    let typed = curve::with_control(start, CurveControl::Y1, 7.0);
    assert!(close(curve::unit_controls(typed), [0.42, 2.0, 0.58, 1.0]));
}

#[test]
fn handles_snap_to_quarters_and_thirds_per_axis() {
    let (point, snap) = curve::snap_point([0.335, 0.74], DEFAULT_CURVE_SNAP);
    assert!((point[0] - 1.0 / 3.0).abs() < 1e-9 && (point[1] - 0.75).abs() < 1e-9);
    assert_eq!(snap.x, Some(21_845));
    assert_eq!(snap.y, Some(49_152));
    let (point, snap) = curve::snap_point([0.41, 1.52], DEFAULT_CURVE_SNAP);
    assert!((point[0] - 0.41).abs() < 1e-9, "nothing near 0.41");
    assert_eq!(snap.x, None);
    assert_eq!(snap.unit_y(), Some(1.5), "the overshoot mark");
}

#[test]
fn seeds_keep_custom_curves_and_approximate_named_easings() {
    let custom = CurvePreset::Back.curve();
    assert_eq!(curve_seed(Easing::CubicBezier(custom)), custom);
    assert_eq!(
        curve_seed(Easing::CubicOut),
        fm_animation::EasingPreset::CubicOut.to_bezier().unwrap()
    );
    assert_eq!(curve_seed(Easing::Hold), default_curve());
    assert_eq!(curve_seed(Easing::Step), default_curve());
    // Presets overshoot and anticipate where they say they do.
    assert!(curve::unit_controls(CurvePreset::Back.curve())[1] > 1.0);
    assert!(curve::unit_controls(CurvePreset::Anticipate.curve())[1] < 0.0);
    let ids: std::collections::BTreeSet<_> =
        CurvePreset::ALL.iter().map(|preset| preset.id()).collect();
    assert_eq!(ids.len(), CurvePreset::ALL.len());
}

#[test]
fn dragging_a_segment_handle_sets_its_easing_in_one_undo_step() {
    let mut document = document(100, &[(Property::Opacity, &[(0, 0), (40, 255), (80, 0)])]);
    let segment = key(Property::Opacity, 0);
    let generation = document.generation();
    let start = document
        .begin_curve_drag(CurveTarget::Segment(segment), CurveHandle::Out)
        .unwrap();
    // Linear seeds its handles on the diagonal at thirds.
    assert!(close(
        curve::unit_controls(start),
        [1.0 / 3.0, 1.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0]
    ));
    assert!(document.drag().unwrap().is_curve());
    // Snapped: 0.26 -> 0.25, 0.02 -> 0.
    let snap = document
        .update_curve_drag([0.26, 0.02], Modifiers::NONE, DEFAULT_CURVE_SNAP)
        .unwrap();
    assert_eq!(snap.unit_x(), Some(0.25));
    assert_eq!(snap.unit_y(), Some(0.0));
    assert!(close(
        bezier(&document, Property::Opacity, 0),
        [0.25, 0.0, 2.0 / 3.0, 2.0 / 3.0]
    ));
    // Shift moves freely, and overshoot is clamped to the engine's limit.
    let snap = document
        .update_curve_drag([0.26, 2.4], Modifiers::SHIFT, DEFAULT_CURVE_SNAP)
        .unwrap();
    assert_eq!(snap, CurveSnap::NONE);
    let controls = bezier(&document, Property::Opacity, 0);
    assert!((controls[0] - 0.26).abs() < 1e-4 && (controls[1] - MAX_CURVE_Y).abs() < 1e-9);
    assert!(
        document.generation() > generation,
        "the draft shows it live"
    );
    assert_eq!(document.undo_depth(), 0);
    // Timeline pointer updates leave a curve drag alone; edits wait.
    assert_eq!(
        document.update_drag(Pointer::at(30), Modifiers::NONE),
        Ok(None)
    );
    assert_eq!(document.undo(), Err(AnimEditorError::DragActive));
    assert_eq!(
        document.set_curve(CurveTarget::Segment(segment), default_curve()),
        Err(AnimEditorError::DragActive)
    );
    let (handle, _, _) = document.curve_drag().unwrap();
    assert_eq!(handle, CurveHandle::Out);
    let outcome = document.commit_drag().unwrap();
    assert!(outcome.edited);
    assert_eq!(document.undo_depth(), 1, "the whole drag is one step");
    // Only the segment's outgoing key changed.
    assert_eq!(easing(&document, Property::Opacity, 40), Easing::Linear);
    document.undo().unwrap();
    assert_eq!(easing(&document, Property::Opacity, 0), Easing::Linear);
}

#[test]
fn a_cancelled_curve_drag_restores_the_easing() {
    let mut document = document(60, &[(Property::Opacity, &[(0, 0), (40, 255)])]);
    document
        .begin_curve_drag(
            CurveTarget::Segment(key(Property::Opacity, 0)),
            CurveHandle::In,
        )
        .unwrap();
    document
        .update_curve_drag([0.9, 1.4], Modifiers::SHIFT, DEFAULT_CURVE_SNAP)
        .unwrap();
    document.cancel_drag().unwrap();
    assert_eq!(easing(&document, Property::Opacity, 0), Easing::Linear);
    assert_eq!(document.undo_depth(), 0);
    assert_eq!(
        document.update_curve_drag([0.5, 0.5], Modifiers::NONE, DEFAULT_CURVE_SNAP),
        Err(AnimEditorError::NoDrag)
    );
}

#[test]
fn the_last_key_of_a_track_has_no_segment() {
    let mut document = document(60, &[(Property::Opacity, &[(0, 0), (40, 255)])]);
    let last = key(Property::Opacity, 40);
    assert_eq!(document.segment(last), None);
    let error = document
        .begin_curve_drag(CurveTarget::Segment(last), CurveHandle::Out)
        .unwrap_err();
    assert_eq!(error.msg_key(), "anim-editor-no-segment");
    assert_eq!(
        document.set_curve(CurveTarget::Selection, default_curve()),
        Err(AnimEditorError::EmptySelection)
    );
    assert!(document.drag().is_none());
}

#[test]
fn numeric_entry_and_presets_edit_the_selection_in_one_step_each() {
    let mut document = document(
        60,
        &[
            (Property::Opacity, &[(0, 0), (40, 255)]),
            (Property::PositionX, &[(0, -100), (40, 0)]),
        ],
    );
    document.set_selection(&fm_animation::KeySelection::from_iter([
        key(Property::Opacity, 0),
        key(Property::PositionX, 0),
    ]));
    document
        .apply_curve_preset(CurveTarget::Selection, CurvePreset::Back)
        .unwrap();
    assert_eq!(document.undo_depth(), 1);
    for property in [Property::Opacity, Property::PositionX] {
        assert!(close(
            bezier(&document, property, 0),
            [0.34, 1.56, 0.64, 1.0]
        ));
    }
    document
        .set_curve_control(CurveTarget::Selection, CurveControl::Y1, 1.2)
        .unwrap();
    assert_eq!(document.undo_depth(), 2);
    assert!(close(
        bezier(&document, Property::PositionX, 0),
        [0.34, 1.2, 0.64, 1.0]
    ));
    // Out-of-range numbers are clamped.
    document
        .set_curve_control(CurveTarget::Selection, CurveControl::X2, 1.5)
        .unwrap();
    assert!((bezier(&document, Property::Opacity, 0)[2] - 1.0).abs() < 1e-9);
    document.undo().unwrap();
    document.undo().unwrap();
    assert!(close(
        bezier(&document, Property::Opacity, 0),
        [0.34, 1.56, 0.64, 1.0]
    ));
}

#[test]
fn segments_report_influence_and_speed() {
    let mut document = document(100, &[(Property::PositionX, &[(10, 0), (60, 500)])]);
    let segment = document.segment_at(track(Property::PositionX), 30).unwrap();
    assert_eq!(segment.key, key(Property::PositionX, 10));
    assert_eq!(segment.frames(), 50);
    assert_eq!(document.segment_at(track(Property::PositionX), 60), None);
    assert_eq!(document.segment_at(track(Property::PositionX), 5), None);
    let curve = curve::curve_from_unit([0.25, 0.5, 0.5, 1.0]);
    assert!((Segment::<u32>::influence(curve, CurveHandle::Out) - 0.25).abs() < 1e-4);
    assert!((Segment::<u32>::influence(curve, CurveHandle::In) - 0.5).abs() < 1e-4);
    // Slope 2 over 500 px in 50 frames: 20 px per frame leaving the key.
    let speed = segment.speed_per_frame(curve, CurveHandle::Out).unwrap();
    assert!((speed - 20.0).abs() < 0.01, "{speed}");
    // Arriving flat: zero speed.
    let arriving = segment.speed_per_frame(curve, CurveHandle::In).unwrap();
    assert!(arriving.abs() < 1e-9);
    let jump = curve::curve_from_unit([0.0, 0.5, 0.5, 1.0]);
    assert_eq!(segment.speed_per_frame(jump, CurveHandle::Out), None);
    document.set_selection(&fm_animation::KeySelection::from_iter([key(
        Property::PositionX,
        10,
    )]));
    assert_eq!(
        document.curve(CurveTarget::Selection).unwrap(),
        curve_seed(Easing::Linear)
    );
}

#[test]
fn sampling_a_custom_curve_matches_the_engine() {
    let curve = CurvePreset::EaseInOut.curve();
    assert!(curve::sample_curve(curve, 0.0).abs() < 1e-9);
    assert!((curve::sample_curve(curve, 1.0) - 1.0).abs() < 1e-9);
    assert!((curve::sample_curve(curve, 0.5) - 0.5).abs() < 1e-3);
    let [x, y] = curve::bezier_point(curve, 1.0);
    assert!((x - 1.0).abs() < 1e-9 && (y - 1.0).abs() < 1e-9);
}
