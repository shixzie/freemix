// The assertions compare exact reference values.
#![allow(clippy::float_cmp)]

use core::time::Duration;
use std::sync::Arc;

use fm_grade::{
    CompiledGrade, CubeLut, CurveChannel, Grade, GradeError, GradeLut, HueCurveKind, Wheel,
    WheelKind, builtin_preset,
};

use super::*;

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn lut_grade() -> Grade {
    Grade {
        lut: Some(GradeLut::new("asset://luts/show.cube", 1000)),
        ..Grade::IDENTITY
    }
}

// ---- document ------------------------------------------------------------

#[test]
fn edits_are_validated_and_undoable() {
    let mut document = GradeDocument::default();
    document.set_param(GradeParam::Exposure, 500).unwrap();
    assert_eq!(document.grade().exposure_millistops, 500);
    assert!(document.is_dirty());
    assert_eq!(document.undo_depth(), 1);

    // Out of range values clamp; an invalid wheel is refused unchanged.
    document.set_param(GradeParam::Saturation, 9_999).unwrap();
    assert_eq!(document.grade().saturation_permille, 3000);
    let generation = document.generation();
    let error = document
        .set_wheel(WheelKind::Lift, Wheel::new(1000, 1000, 0))
        .unwrap_err();
    assert!(matches!(
        error,
        GradeEditorError::Invalid(GradeError::WheelRadius { .. })
    ));
    assert_eq!(error.msg_key(), "grade-editor-invalid");
    assert_eq!(document.generation(), generation);

    // An edit that changes nothing records nothing.
    document.set_param(GradeParam::Saturation, 3000).unwrap();
    assert_eq!(document.undo_depth(), 2);

    document.undo().unwrap();
    document.undo().unwrap();
    assert_eq!(document.grade(), &Grade::IDENTITY);
    assert!(
        !document.is_dirty(),
        "undo back to the saved grade is clean"
    );
    assert_eq!(document.undo(), Err(GradeEditorError::NothingToUndo));
    document.redo().unwrap();
    assert_eq!(document.grade().exposure_millistops, 500);
    document.set_param(GradeParam::Tint, 10).unwrap();
    assert_eq!(document.redo(), Err(GradeEditorError::NothingToRedo));
}

#[test]
fn a_gesture_is_one_undo_step_and_can_be_cancelled() {
    let mut document = GradeDocument::default();
    document.begin_gesture().unwrap();
    assert_eq!(
        document.begin_gesture(),
        Err(GradeEditorError::GestureActive)
    );
    assert_eq!(document.undo(), Err(GradeEditorError::GestureActive));
    assert!(!document.can_undo());
    for value in [100, 200, 300] {
        document
            .update_gesture(|grade| {
                GradeParam::Contrast.set(grade, value);
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(document.grade().contrast.amount, 300);
    assert!(document.commit_gesture().unwrap());
    assert_eq!(document.undo_depth(), 1);
    document.undo().unwrap();
    assert_eq!(document.grade().contrast.amount, 0);

    document.begin_gesture().unwrap();
    document
        .update_gesture(|grade| {
            grade.exposure_millistops = 1000;
            Ok(())
        })
        .unwrap();
    document.cancel_gesture().unwrap();
    assert_eq!(document.grade().exposure_millistops, 0);
    assert_eq!(document.commit_gesture(), Err(GradeEditorError::NoGesture));
    assert_eq!(
        document.update_gesture(|_| Ok(())),
        Err(GradeEditorError::NoGesture)
    );
    // A gesture that changed nothing records nothing.
    document.begin_gesture().unwrap();
    assert!(!document.commit_gesture().unwrap());
    assert_eq!(document.undo_depth(), 0);
}

#[test]
fn undo_history_is_capped() {
    let mut document = GradeDocument::default();
    for value in 1..=(i32::try_from(GRADE_UNDO_LIMIT).unwrap() + 20) {
        document.set_param(GradeParam::Exposure, value).unwrap();
    }
    assert_eq!(document.undo_depth(), GRADE_UNDO_LIMIT);
    while document.undo().is_ok() {}
    assert_eq!(document.grade().exposure_millistops, 20);
}

#[test]
fn presets_replace_the_grade_but_keep_the_lut_and_bypass() {
    let mut document = GradeDocument::new(lut_grade(), 3);
    document.set_bypass(true).unwrap();
    document.set_param(GradeParam::Exposure, 700).unwrap();
    let warm = builtin_preset("warm").unwrap();
    document.apply_preset(warm).unwrap();
    assert_eq!(document.grade().white_balance, warm.grade.white_balance);
    assert_eq!(document.grade().exposure_millistops, 0);
    assert!(document.grade().lut.is_some());
    assert!(document.is_bypassed());
    assert_eq!(
        document.matching_preset().map(|preset| preset.id),
        Some("warm")
    );
    document.set_param(GradeParam::Tint, 1).unwrap();
    assert!(document.matching_preset().is_none());
    // One step per action.
    assert_eq!(document.undo_depth(), 4);
}

#[test]
fn sections_and_params_reset_to_identity() {
    let mut document = GradeDocument::new(builtin_preset("faded").unwrap().grade.clone(), 1);
    assert!(!GradeSection::Curves.is_default(document.grade()));
    document.reset_section(GradeSection::Curves).unwrap();
    assert!(document.grade().curves.is_identity());
    assert!(GradeSection::Curves.is_default(document.grade()));
    document
        .reset_param(GradeParam::WheelMaster(WheelKind::Lift))
        .unwrap();
    assert_eq!(document.grade().wheels.lift, Wheel::NEUTRAL);
    document.reset_param(GradeParam::Saturation).unwrap();
    assert_eq!(document.grade().saturation_permille, 1000);
    document.set_bypass(true).unwrap();
    document.reset_section(GradeSection::All).unwrap();
    assert!(document.is_bypassed(), "reset all keeps the bypass state");
    let mut identity = Grade::IDENTITY;
    identity.enabled = false;
    assert_eq!(document.grade(), &identity);
    // Strength needs a LUT; resetting the LUT drops it.
    let mut document = GradeDocument::new(lut_grade(), 1);
    document.set_param(GradeParam::LutStrength, 250).unwrap();
    assert_eq!(GradeParam::LutStrength.get(document.grade()), 250);
    document.reset_param(GradeParam::LutStrength).unwrap();
    assert_eq!(GradeParam::LutStrength.get(document.grade()), 1000);
    document.reset_section(GradeSection::Lut).unwrap();
    assert!(document.grade().lut.is_none());
    GradeParam::LutStrength.set(&mut Grade::IDENTITY.clone(), 5);
}

#[test]
fn copy_and_paste_a_grade() {
    let mut document = GradeDocument::default();
    assert_eq!(document.paste(), Err(GradeEditorError::EmptyClipboard));
    document.set_param(GradeParam::Saturation, 1500).unwrap();
    document.copy();
    document.reset_section(GradeSection::All).unwrap();
    document.set_bypass(true).unwrap();
    document.paste().unwrap();
    assert_eq!(document.grade().saturation_permille, 1500);
    assert!(document.is_bypassed(), "paste keeps the bypass state");
    document.undo().unwrap();
    assert_eq!(document.grade().saturation_permille, 1000);
}

#[test]
fn ab_toggle_flips_between_two_grades() {
    let mut document = GradeDocument::default();
    assert_eq!(
        document.recall_slot(AbSlot::B),
        Err(GradeEditorError::EmptySlot(AbSlot::B))
    );
    document.set_param(GradeParam::Exposure, 300).unwrap();
    // First toggle: B starts as a copy, so the picture does not change.
    document.toggle_ab().unwrap();
    assert_eq!(document.active_slot(), AbSlot::B);
    assert_eq!(document.grade().exposure_millistops, 300);
    let steps = document.undo_depth();
    // Tweak B, then flip back to A and again to B.
    document.set_param(GradeParam::Exposure, -300).unwrap();
    document.toggle_ab().unwrap();
    assert_eq!(document.active_slot(), AbSlot::A);
    assert_eq!(document.grade().exposure_millistops, 300);
    document.toggle_ab().unwrap();
    assert_eq!(document.grade().exposure_millistops, -300);
    assert_eq!(document.slot(AbSlot::A).unwrap().exposure_millistops, 300);
    assert_eq!(document.undo_depth(), steps + 3);
    document.clear_slots();
    assert!(document.slot(AbSlot::A).is_none());
    assert_eq!(document.active_slot(), AbSlot::A);
}

#[test]
fn authority_reset_and_save() {
    let mut document = GradeDocument::default();
    document.set_param(GradeParam::Exposure, 300).unwrap();
    document.mark_saved(7);
    assert!(!document.is_dirty());
    assert_eq!(document.base_revision(), 7);
    assert!(document.can_undo(), "history is kept across a save");
    document.undo().unwrap();
    assert!(document.is_dirty());
    document.reset_from_authority(lut_grade(), 9);
    assert!(!document.is_dirty());
    assert!(!document.can_undo());
    assert_eq!(document.base_revision(), 9);
}

// ---- wheels ---------------------------------------------------------------

#[test]
fn puck_position_round_trips_and_clamps_to_the_disc() {
    let wheel = Wheel::new(300, -400, 0);
    assert_eq!(puck_offset(wheel), [0.3, -0.4]);
    assert_eq!(direction_from_offset([0.3, -0.4]), (300, -400));
    let (x, y) = direction_from_offset([3.0, 4.0]);
    assert_eq!((x, y), (600, 800));
    let (x, y) = direction_from_offset([0.7072, 0.7072]);
    assert!(i32::from(x).pow(2) + i32::from(y).pow(2) <= 1_000_000);
    assert_eq!(direction_from_offset([f32::NAN, 0.0]), (0, 0));
}

#[test]
fn wheel_drags_are_relative_with_fine_adjust() {
    // A 100 px wheel: 50 px right and 20 px up moves the puck by (0.5, 0.2).
    let mut drag = WheelDrag::new(Wheel::NEUTRAL, [200.0, 200.0], 100.0, false);
    assert_eq!(drag.update([250.0, 180.0], false), (500, 200));
    // Holding fine re-anchors: no jump, then a tenth of the motion.
    assert_eq!(drag.update([250.0, 180.0], true), (500, 200));
    assert_eq!(drag.update([300.0, 180.0], true), (550, 200));
    // Releasing fine re-anchors too; far outside stays on the rim.
    assert_eq!(drag.update([300.0, 180.0], false), (550, 200));
    let (x, y) = drag.update([2_000.0, 180.0], false);
    assert!(x > 990 && y.abs() < 200, "{x} {y}");

    let mut value = ValueDrag::new(0, (-1000, 1000), 200.0, 10.0, false);
    assert_eq!(value.update(60.0, false), 500);
    assert_eq!(value.update(60.0, true), 500);
    assert_eq!(value.update(160.0, true), 600);
    assert_eq!(value.update(160.0, false), 600);
    assert_eq!(value.update(10_000.0, false), 1000);
}

#[test]
fn wheel_colours_follow_the_cb_cr_directions() {
    let up = wheel_color([0.0, 1.0]);
    assert!(up[0] > up[2], "Cr up is red: {up:?}");
    let right = wheel_color([1.0, 0.0]);
    assert!(right[2] > right[0], "Cb right is blue: {right:?}");
    let centre = wheel_color([0.0, 0.0]);
    assert_eq!(centre[0], centre[1]);
    assert_eq!(centre[1], centre[2]);
}

// ---- curves ---------------------------------------------------------------

#[test]
fn rgb_curve_points_add_move_and_delete_within_bounds() {
    let master = CurveTarget::Rgb(CurveChannel::Master);
    let mut grade = Grade::IDENTITY;
    let index = master.add_point(&mut grade, [0.5, 0.6]).unwrap();
    assert_eq!(index, 1);
    assert_eq!(
        master.points(&grade),
        vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]
    );
    assert_eq!(
        master.add_point(&mut grade, [0.505, 0.2]),
        Err(GradeEditorError::PointTaken)
    );
    assert_eq!(
        master.add_point(&mut grade, [1.0, 0.2]),
        Err(GradeEditorError::PointTaken),
        "endpoints are taken"
    );
    // Moving past a neighbour stops a gap short of it.
    master.move_point(&mut grade, 1, [0.999, 0.7]).unwrap();
    assert_eq!(grade.curves.master.points()[1].x, 9_900);
    // Endpoints keep x.
    master.move_point(&mut grade, 0, [0.3, 0.1]).unwrap();
    assert_eq!(master.points(&grade)[0], [0.0, 0.1]);
    grade.validate().unwrap();
    assert_eq!(
        master.delete_point(&mut grade, 0),
        Err(GradeEditorError::EndpointLocked)
    );
    assert_eq!(
        master.delete_point(&mut grade, 9),
        Err(GradeEditorError::UnknownPoint)
    );
    assert_eq!(
        master.move_point(&mut grade, 9, [0.0, 0.0]),
        Err(GradeEditorError::UnknownPoint)
    );
    master.delete_point(&mut grade, 1).unwrap();
    assert_eq!(master.len(&grade), 2);
    // Fill to the limit.
    let red = CurveTarget::Rgb(CurveChannel::Red);
    for step in 1..=14_u8 {
        red.add_point(&mut grade, [f32::from(step) / 15.0, 0.5])
            .unwrap();
    }
    assert_eq!(
        red.add_point(&mut grade, [0.99, 0.5]),
        Err(GradeEditorError::CurveFull { max: 16 })
    );
    grade.validate().unwrap();
}

#[test]
fn hue_curves_seed_neutral_anchors_and_wrap() {
    let hue_sat = CurveTarget::Hue(HueCurveKind::HueVsSaturation);
    let mut grade = Grade::IDENTITY;
    assert!(hue_sat.is_empty(&grade));
    // 1/3 turn is green, exactly on an anchor: the anchor is replaced.
    let index = hue_sat.add_point(&mut grade, [1.0 / 3.0, 1.0]).unwrap();
    let points = grade.hue_curves.hue_vs_saturation.points().to_vec();
    assert_eq!(points.len(), 6);
    assert_eq!(points[index].position, 1200);
    assert_eq!(points[index].value, 1000);
    grade.validate().unwrap();
    // The curve is neutral at the opposite hue and full at green.
    let samples = hue_sat.sample(&grade, 361);
    assert!((samples[120] - 1.0).abs() < 1e-4, "{}", samples[120]);
    assert!((samples[300] - 0.5).abs() < 1e-4);
    // Near 360° wraps onto the anchor at 0°.
    assert_eq!(
        hue_sat.add_point(&mut grade, [0.995, 0.5]),
        Err(GradeEditorError::PointTaken)
    );
    // Hue points can all be deleted.
    assert!(hue_sat.can_delete(&grade, 0));
    hue_sat.delete_point(&mut grade, 0).unwrap();
    assert_eq!(hue_sat.len(&grade), 5);

    let luma_sat = CurveTarget::Hue(HueCurveKind::LumaVsSaturation);
    let index = luma_sat.add_point(&mut grade, [0.5, 0.25]).unwrap();
    assert_eq!(index, 1);
    assert_eq!(luma_sat.len(&grade), 3);
    luma_sat.move_point(&mut grade, 1, [2.0, 0.0]).unwrap();
    let points = grade.hue_curves.luma_vs_saturation.points();
    assert_eq!(points[1].position, 9_900);
    assert_eq!(points[1].value, -1000);
    grade.validate().unwrap();
}

#[test]
fn curve_samples_match_the_grade_evaluation() {
    let master = CurveTarget::Rgb(CurveChannel::Master);
    let mut grade = Grade::IDENTITY;
    master.add_point(&mut grade, [0.25, 0.4]).unwrap();
    master.add_point(&mut grade, [0.75, 0.7]).unwrap();
    let samples = master.sample(&grade, 256);
    let compiled = CompiledGrade::new(&grade, None).unwrap();
    for (code, sample) in (0..=255_u8).zip(&samples) {
        let x = f32::from(code) / 255.0;
        let graded = compiled.evaluate([x; 3]);
        assert!(
            (graded[0] - sample).abs() < 1e-5,
            "{code}: {graded:?} {sample}"
        );
    }
    assert_eq!(master.hit(&grade, [0.26, 0.41], [0.03, 0.03]), Some(1));
    assert_eq!(master.hit(&grade, [0.5, 0.9], [0.03, 0.03]), None);
    assert!(
        CurveTarget::Hue(HueCurveKind::HueVsHue)
            .sample(&grade, 8)
            .iter()
            .all(|&y| (y - 0.5).abs() < f32::EPSILON)
    );
}

#[test]
fn document_curve_edits_are_single_steps() {
    let mut document = GradeDocument::default();
    let blue = CurveTarget::Rgb(CurveChannel::Blue);
    let index = document.add_curve_point(blue, [0.5, 0.3]).unwrap();
    document.begin_gesture().unwrap();
    for y in [0.32, 0.35, 0.4] {
        document
            .update_gesture(|grade| blue.move_point(grade, index, [0.5, y]))
            .unwrap();
    }
    document.commit_gesture().unwrap();
    assert_eq!(document.undo_depth(), 2);
    document.delete_curve_point(blue, index).unwrap();
    assert!(document.grade().curves.blue.is_identity());
    assert_eq!(
        document.delete_curve_point(blue, 0),
        Err(GradeEditorError::EndpointLocked)
    );
    assert_eq!(document.undo_depth(), 3);
}

// ---- previews --------------------------------------------------------------

#[test]
fn preview_cache_is_keyed_by_grade_and_source() {
    let mut preview = GradePreview::new(4);
    assert_eq!(preview.graded(&Grade::IDENTITY).unwrap(), None);
    preview.set_source(bars(70, 10));
    let identity = preview.graded(&Grade::IDENTITY).unwrap().unwrap();
    assert!(Arc::ptr_eq(&identity, preview.source().unwrap()));
    let punchy = builtin_preset("punchy").unwrap().grade.clone();
    let first = preview.graded(&punchy).unwrap().unwrap();
    let again = preview.graded(&punchy).unwrap().unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    assert_eq!(preview.cache_stats(), (1, 2));
    // Exactly the compiled grade's output.
    let compiled = CompiledGrade::new(&punchy, None).unwrap();
    let mut expected = bars(70, 10).into_pixels();
    compiled.apply_rgba8_in_place(&mut expected);
    assert_eq!(first.pixels(), expected.as_slice());
    // Eviction beyond capacity, least recently used first.
    for exposure in 1..=4 {
        let grade = Grade {
            exposure_millistops: exposure * 100,
            ..Grade::IDENTITY
        };
        preview.graded(&grade).unwrap();
    }
    preview.graded(&punchy).unwrap();
    assert_eq!(preview.cache_stats(), (1, 7));
    // A new source drops the cache.
    preview.set_source(bars(7, 1));
    preview.graded(&punchy).unwrap();
    assert_eq!(preview.cache_stats(), (1, 8));
    assert!(matches!(
        preview.graded(&Grade {
            saturation_permille: 5000,
            ..Grade::IDENTITY
        }),
        Err(GradeError::Saturation(5000))
    ));
}

#[test]
fn preview_uses_supplied_luts_and_reports_missing_ones() {
    let mut preview = GradePreview::default();
    preview.set_source(RgbaImage::solid(2, 2, [200, 100, 50, 255]));
    let grade = lut_grade();
    assert!(preview.lut_missing(&grade));
    let without = preview.graded(&grade).unwrap().unwrap();
    assert_eq!(without.pixel(0, 0), Some([200, 100, 50, 255]));
    // An inverting 2³ LUT.
    let entries = (0..8)
        .map(|index| {
            let bit = |shift: u32| if index >> shift & 1 == 1 { 0.0 } else { 1.0 };
            [bit(0), bit(1), bit(2)]
        })
        .collect();
    let lut = CubeLut::new_3d(2, entries, [0.0; 3], [1.0; 3]).unwrap();
    preview.set_lut("asset://luts/show.cube", Arc::new(lut));
    assert!(!preview.lut_missing(&grade));
    assert_eq!(
        preview.lut_uris().collect::<Vec<_>>(),
        ["asset://luts/show.cube"]
    );
    let with = preview.graded(&grade).unwrap().unwrap();
    assert_eq!(with.pixel(1, 1), Some([55, 155, 205, 255]));
}

#[test]
fn before_after_compositing() {
    let before = RgbaImage::solid(10, 2, [0, 0, 0, 255]);
    let after = RgbaImage::solid(10, 2, [255, 255, 255, 255]);
    let wipe = compose_wipe(&before, &after, 0.3);
    assert_eq!(wipe.pixel(2, 1), Some([0, 0, 0, 255]));
    assert_eq!(wipe.pixel(3, 1), Some([255, 255, 255, 255]));
    assert_eq!(
        compose_wipe(&before, &after, -1.0).pixel(0, 0).unwrap()[0],
        255
    );
    assert_eq!(
        compose_wipe(&before, &after, 2.0).pixel(9, 0).unwrap()[0],
        0
    );
    let side = compose_side_by_side(&before, &RgbaImage::solid(4, 3, [9, 9, 9, 255]));
    assert_eq!((side.width(), side.height()), (14, 3));
    assert_eq!(side.pixel(9, 0), Some([0, 0, 0, 255]));
    assert_eq!(side.pixel(10, 2), Some([9, 9, 9, 255]));
    assert_eq!(side.pixel(0, 2), Some([0, 0, 0, 0]));
    assert!(RgbaImage::new(2, 2, vec![0; 15]).is_none());
    let small = bars(700, 100).fit_within(70, 70);
    assert_eq!((small.width(), small.height()), (70, 10));
    assert_eq!(small.pixel(5, 5), Some([191, 191, 191, 255]));
}

#[test]
fn recompute_scheduler_debounces_latest_wins_with_a_max_wait() {
    let mut scheduler = RecomputeScheduler::new(ms(30), ms(100));
    assert_eq!(scheduler.poll(ms(0)), None);
    scheduler.request(1_u64, ms(0));
    assert_eq!(scheduler.poll(ms(29)), None);
    scheduler.request(2, ms(20));
    assert_eq!(scheduler.next_deadline(), Some(ms(50)));
    assert_eq!(scheduler.poll(ms(49)), None);
    assert_eq!(scheduler.poll(ms(50)), Some(2), "latest wins");
    assert_eq!(scheduler.current(), Some(2));
    scheduler.request(2, ms(60));
    assert_eq!(scheduler.pending(), None, "already current");
    // A continuous drag still refreshes after the max wait.
    for (step, now) in (100..=200).step_by(10).enumerate() {
        scheduler.request(10 + step as u64, ms(now));
        if now < 200 {
            assert_eq!(scheduler.poll(ms(now)), None, "{now}");
        }
    }
    assert_eq!(scheduler.poll(ms(200)), Some(20));
    scheduler.request_now(30);
    assert_eq!(scheduler.next_deadline(), Some(Duration::ZERO));
    assert_eq!(scheduler.poll(ms(0)), Some(30));
    scheduler.reset();
    assert_eq!(scheduler.current(), None);
}

#[test]
fn auto_balance_neutralizes_a_colour_cast() {
    // A warm cast over a grey ramp.
    let image = RgbaImage::from_fn(64, 8, |x, _| {
        let level = u8::try_from(40 + x * 2).unwrap();
        [
            level.saturating_add(12),
            level,
            level.saturating_sub(10),
            255,
        ]
    });
    let balance = neutral_white_balance(&image).unwrap();
    assert!(balance.temperature < -100, "{balance:?}");
    let grade = Grade {
        white_balance: balance,
        ..Grade::IDENTITY
    };
    let compiled = CompiledGrade::new(&grade, None).unwrap();
    let mut pixels = image.into_pixels();
    compiled.apply_rgba8_in_place(&mut pixels);
    let mut sum = [0.0_f64; 3];
    for pixel in pixels.as_chunks::<4>().0 {
        for (total, &code) in sum.iter_mut().zip(pixel) {
            *total += f64::from(fm_grade::srgb_eotf(f32::from(code) / 255.0));
        }
    }
    let spread =
        sum.iter().copied().fold(f64::MIN, f64::max) / sum.iter().copied().fold(f64::MAX, f64::min);
    assert!(spread < 1.03, "{sum:?}");
    assert_eq!(
        neutral_white_balance(&RgbaImage::solid(4, 4, [0, 0, 0, 255])),
        None
    );
    let grey = neutral_white_balance(&RgbaImage::solid(4, 4, [128; 4])).unwrap();
    assert_eq!(grey, fm_grade::WhiteBalance::NEUTRAL);
}

// ---- scopes ---------------------------------------------------------------

#[test]
fn scopes_on_colour_bars() {
    let image = bars(140, 20);
    let config = ScopeConfig {
        columns: 7,
        levels: 100,
        vectorscope: 100,
        bins: 256,
        ..ScopeConfig::default()
    };
    let scopes = compute_scopes(&image, config);
    assert_eq!(scopes.samples, 140 * 20);
    for (column, rgb) in BARS_75.iter().enumerate() {
        let normalized = rgb.map(|code| f32::from(code) / 255.0);
        // Waveform: each bar is one luma level in its own column.
        let level = bucket(luma(normalized), 100);
        assert_eq!(scopes.waveform.get(column, level), 20 * 20, "bar {column}");
        // Parade: each channel at 0 or 75%.
        for (channel, grid) in scopes.parade.iter().enumerate() {
            assert_eq!(grid.get(column, bucket(normalized[channel], 100)), 400);
        }
        // Vectorscope: each bar lands on its BT.709 chroma target.
        let (x, y) = vectorscope_cell(chroma(normalized), 100);
        assert!(scopes.vectorscope.get(x, y) >= 400, "bar {column}");
    }
    // Every grey pixel sits in the vectorscope's centre.
    assert_eq!(scopes.vectorscope.get(50, 50), 400);
    // Pure 75% red is up and to the left: Cb < 0, Cr > 0.
    let red = chroma([0.75, 0.0, 0.0]);
    assert!(red[0] < 0.0 && red[1] > 0.0);
    // Histogram: three of seven bars have red at 75%.
    assert_eq!(scopes.histogram.red[191], 3 * 400 + 400);
    assert_eq!(scopes.histogram.red[0], 3 * 400);
    assert_eq!(scopes.histogram.luma.iter().sum::<u32>(), 2_800);
    assert_eq!(scopes.histogram.max(), 1_600);
    assert_eq!(scopes.waveform.total(), 2_800);
    assert_eq!(scopes.waveform.max(), 400);
}

#[test]
fn scopes_subsample_large_frames_and_handle_empty_ones() {
    let image = sample_still(1920, 1080);
    let scopes = compute_scopes(&image, ScopeConfig::default());
    assert!(scopes.samples <= 256 * 144 && scopes.samples > 10_000);
    let empty = compute_scopes(&RgbaImage::solid(0, 0, [0; 4]), ScopeConfig::default());
    assert_eq!(empty.samples, 0);
    assert_eq!(empty.waveform.width(), 128);
    assert_eq!(empty.waveform.height(), 64);
}
