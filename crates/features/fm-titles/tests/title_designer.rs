use fm_titles::fm_animation::{
    ClipError, ClipPosition, ClipState, Keyframe, Phase, Property, TrackKey, Value,
};
use fm_titles::{
    AssetCatalog, Bounds, ClipMask, Color, CubicBezier, DesignEdit, DesignEditor, DesignError,
    EASE_ONE, Easing, EasingError, Element, ElementId, ElementKind, Group, GroupTransform,
    MAX_GROUP_DEPTH, NodeSlot, PlaybackError, PlaybackOutcome, PlaybackPhase, ReferenceRenderer,
    Style, TemplateId, TitleDesign, TitleId, TitlePlayback, ease_between, progress_q16,
};
use std::num::NonZeroU128;

const RED: Color = Color::new(255, 0, 0, 255);

fn id(value: u128) -> ElementId {
    ElementId::new(NonZeroU128::new(value).unwrap())
}

fn bounds(x: i32, y: i32, width: u32, height: u32) -> Bounds {
    Bounds {
        x,
        y,
        width,
        height,
    }
}

fn rect(element: u128, area: Bounds, fill: Color) -> Element {
    Element {
        id: id(element),
        name: format!("rect-{element}"),
        bounds: area,
        z_index: 0,
        visible: true,
        style: Style {
            fill,
            ..Style::default()
        },
        color_field: None,
        kind: ElementKind::Rectangle,
    }
}

fn group(group: u128, transform: GroupTransform, mask: Option<ClipMask>) -> Group {
    Group {
        transform,
        mask,
        ..Group::new(id(group), format!("group-{group}"))
    }
}

fn editor(width: u32, height: u32) -> DesignEditor {
    let design = TitleDesign::new(
        TemplateId::new(NonZeroU128::new(1).unwrap()),
        "designer test",
        width,
        height,
        Color::TRANSPARENT,
        Vec::new(),
    )
    .unwrap();
    DesignEditor::new(design).unwrap()
}

fn add_group(editor: &mut DesignEditor, slot: NodeSlot, value: Group) {
    editor
        .apply(DesignEdit::AddGroup { slot, group: value })
        .unwrap();
}

fn add_rect(editor: &mut DesignEditor, slot: NodeSlot, element: Element) {
    editor
        .apply(DesignEdit::AddElement { slot, element })
        .unwrap();
}

fn keyframe(frame: u32, value: i64, easing: Easing) -> Keyframe {
    Keyframe::new(frame, Value::Scalar(value), easing)
}

fn track(node: u128, phase: Phase, property: Property) -> TrackKey<ElementId> {
    TrackKey::new(id(node), phase, property)
}

fn in_frame(frame: u32) -> ClipState {
    ClipState::In(ClipPosition::frame(frame))
}

/// The frames and scalar values of one track.
fn keys(design: &DesignEditor, key: TrackKey<ElementId>) -> Vec<(u32, i64)> {
    design
        .document()
        .animation()
        .track(key)
        .map(|track| {
            track
                .keys
                .iter()
                .map(|keyframe| match keyframe.value {
                    Value::Scalar(value) => (keyframe.frame, value),
                    Value::Color(_) => (keyframe.frame, i64::MIN),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Renders the design at a timeline sample and returns the alpha plane.
fn render_alpha(design: &TitleDesign, sample: ClipState) -> Vec<u8> {
    render_pixels(design, sample)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| pixel[3])
        .collect()
}

fn render_pixels(design: &TitleDesign, sample: ClipState) -> Vec<u8> {
    let scene = design
        .to_template()
        .instantiate(TitleId::new(NonZeroU128::new(9).unwrap()))
        .unwrap();
    ReferenceRenderer
        .render_composed(&scene, 0, &AssetCatalog::new(), &design.compose(sample))
        .unwrap()
        .frame
        .pixels()
        .to_vec()
}

/// Reference `y(x)` of a cubic bezier in `f64`, solved to full precision.
fn bezier_reference(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    let curve = |a: f64, b: f64, t: f64| {
        let u = 1.0 - t;
        3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
    };
    let (mut low, mut high) = (0.0_f64, 1.0_f64);
    for _ in 0..80 {
        let middle = f64::midpoint(low, high);
        if curve(x1, x2, middle) < x {
            low = middle;
        } else {
            high = middle;
        }
    }
    curve(y1, y2, high)
}

#[test]
fn polynomial_easings_hit_endpoints_symmetry_and_known_values() {
    let half = EASE_ONE / 2;
    let all = [
        Easing::Linear,
        Easing::Step,
        Easing::QuadIn,
        Easing::QuadOut,
        Easing::QuadInOut,
        Easing::CubicIn,
        Easing::CubicOut,
        Easing::CubicInOut,
    ];
    let one = i32::try_from(EASE_ONE).unwrap();
    for easing in all {
        assert_eq!(easing.apply(0), 0, "{easing:?}");
        assert_eq!(easing.apply(EASE_ONE), one, "{easing:?}");
        assert_eq!(easing.apply(EASE_ONE * 4), one, "progress clamps");
        let mut previous = 0;
        for progress in 0..=EASE_ONE {
            let value = easing.apply(progress);
            assert!(
                value >= previous && value <= one,
                "{easing:?} at {progress}"
            );
            previous = value;
        }
    }
    assert_eq!(Easing::Linear.apply(12_345), 12_345);
    assert_eq!(Easing::Step.apply(EASE_ONE - 1), 0);
    assert_eq!(Easing::QuadIn.apply(half), 16_384);
    assert_eq!(Easing::QuadOut.apply(half), 49_152);
    assert_eq!(Easing::QuadInOut.apply(half), 32_768);
    assert_eq!(Easing::CubicIn.apply(half), 8_192);
    assert_eq!(Easing::CubicOut.apply(half), 57_344);
    assert_eq!(Easing::CubicInOut.apply(half), 32_768);
    // One rounding: (1/3)^2 = 7281.7 -> 7282 in Q16.
    assert_eq!(Easing::QuadIn.apply(21_845), 7_282);
    for progress in (0..=EASE_ONE).step_by(97) {
        let mirror = EASE_ONE - progress;
        assert_eq!(
            Easing::QuadOut.apply(progress),
            one - Easing::QuadIn.apply(mirror)
        );
        assert_eq!(
            Easing::CubicOut.apply(progress),
            one - Easing::CubicIn.apply(mirror)
        );
        assert_eq!(
            Easing::CubicInOut.apply(progress) + Easing::CubicInOut.apply(mirror),
            one,
            "in-out is point symmetric at {progress}"
        );
    }
}

#[test]
fn cubic_bezier_is_within_documented_precision_and_validates_controls() {
    let q16 = |value: f64| value * f64::from(EASE_ONE);
    let curves: [(f64, f64, f64, f64); 4] = [
        (0.25, 0.1, 0.25, 1.0),
        (0.42, 0.0, 0.58, 1.0),
        (0.68, -0.6, 0.32, 1.6),
        (0.0, 0.0, 1.0, 1.0),
    ];
    for (x1, y1, x2, y2) in curves {
        let fixed = |value: f64| q16(value).round();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (fx1, fy1, fx2, fy2) = (
            fixed(x1) as u32,
            fixed(y1) as i32,
            fixed(x2) as u32,
            fixed(y2) as i32,
        );
        let curve = CubicBezier::new(fx1, fy1, fx2, fy2).unwrap();
        let easing = Easing::CubicBezier(curve);
        let one = f64::from(EASE_ONE);
        for progress in (0..=EASE_ONE).step_by(31) {
            let reference = bezier_reference(
                f64::from(fx1) / one,
                f64::from(fy1) / one,
                f64::from(fx2) / one,
                f64::from(fy2) / one,
                f64::from(progress) / one,
            );
            let error = (f64::from(easing.apply(progress)) - q16(reference)).abs();
            assert!(
                error <= 9.5,
                "curve {x1},{y1},{x2},{y2} at {progress}: {error}"
            );
        }
        assert_eq!(easing.apply(0), 0);
        assert_eq!(easing.apply(EASE_ONE), i32::try_from(EASE_ONE).unwrap());
    }
    // CSS `ease` at x = 0.5 is 0.8024033877.
    let ease = Easing::CubicBezier(CubicBezier::new(16_384, 6_554, 16_384, 65_536).unwrap());
    assert!((ease.apply(EASE_ONE / 2) - 52_586).abs() <= 2);
    // Overshoot is allowed inside the y range.
    let back = Easing::CubicBezier(CubicBezier::new(22_282, -39_322, 20_972, 104_858).unwrap());
    assert!((1..EASE_ONE).any(|p| back.apply(p) > i32::try_from(EASE_ONE).unwrap()));
    assert!((1..EASE_ONE).any(|p| back.apply(p) < 0));
    assert_eq!(
        CubicBezier::new(EASE_ONE + 1, 0, 0, 0),
        Err(EasingError::ControlXOutOfRange(EASE_ONE + 1))
    );
    assert_eq!(
        CubicBezier::new(0, 0, 0, 3 << 16),
        Err(EasingError::ControlYOutOfRange(3 << 16))
    );
}

#[test]
fn eased_interpolation_is_integer_and_truncates_toward_zero() {
    assert_eq!(progress_q16(5, 10), 32_768);
    assert_eq!(progress_q16(1, 3), 21_845);
    assert_eq!(progress_q16(7, 0), EASE_ONE);
    assert_eq!(progress_q16(12, 10), EASE_ONE);
    assert_eq!(ease_between(0, 100, 32_768), 50);
    assert_eq!(ease_between(0, -100, 21_845), -33);
    assert_eq!(ease_between(10, 0, 65_536), 0);
    assert_eq!(ease_between(i64::MAX - 1, i64::MAX, 131_072), i64::MAX);
}

#[test]
fn nested_group_offsets_compose_exactly_and_render_identically() {
    let mut nested = editor(32, 16);
    add_group(
        &mut nested,
        NodeSlot::root(0),
        group(10, GroupTransform::offset(10, 5), None),
    );
    add_group(
        &mut nested,
        NodeSlot::under(id(10), 0),
        group(11, GroupTransform::offset(3, 2), None),
    );
    add_rect(
        &mut nested,
        NodeSlot::under(id(11), 0),
        rect(1, bounds(0, 0, 4, 3), RED),
    );

    let mut flat = editor(32, 16);
    add_group(
        &mut flat,
        NodeSlot::root(0),
        group(10, GroupTransform::offset(13, 7), None),
    );
    add_rect(
        &mut flat,
        NodeSlot::under(id(10), 0),
        rect(1, bounds(0, 0, 4, 3), RED),
    );

    let mut absolute = editor(32, 16);
    add_rect(
        &mut absolute,
        NodeSlot::root(0),
        rect(1, bounds(13, 7, 4, 3), RED),
    );

    let composition = nested.document().compose(ClipState::Static);
    let placement = composition.placement(id(1)).unwrap();
    assert_eq!((placement.offset_x, placement.offset_y), (13, 7));
    assert_eq!(placement.clips(), &[]);

    let reference = render_pixels(absolute.document(), ClipState::Static);
    assert_eq!(
        render_pixels(nested.document(), ClipState::Static),
        reference
    );
    assert_eq!(render_pixels(flat.document(), ClipState::Static), reference);
    let alpha = render_alpha(nested.document(), ClipState::Static);
    assert_eq!(alpha[7 * 32 + 13], 255);
    assert_eq!(alpha[9 * 32 + 16], 255);
    assert_eq!(alpha[7 * 32 + 12], 0);
    assert_eq!(alpha[10 * 32 + 13], 0);
}

#[test]
fn group_opacity_composes_with_one_rounding() {
    let mut nested = editor(4, 4);
    let half = GroupTransform {
        opacity: 128,
        ..GroupTransform::IDENTITY
    };
    add_group(&mut nested, NodeSlot::root(0), group(10, half, None));
    add_group(
        &mut nested,
        NodeSlot::under(id(10), 0),
        group(11, half, None),
    );
    add_rect(
        &mut nested,
        NodeSlot::under(id(11), 0),
        rect(1, bounds(0, 0, 4, 4), Color::new(255, 255, 255, 255)),
    );
    let composition = nested.document().compose(ClipState::Static);
    let placement = composition.placement(id(1)).unwrap();
    // round(255 * 128 * 128 / 255^2) = round(64.25)
    assert_eq!(placement.compose_opacity(255), 64);
    // round(9 * 128 * 128 / 255^2) = round(2.27); folding one group at a
    // time would give round(round(9 * 128 / 255) * 128 / 255) = 3.
    assert_eq!(placement.compose_opacity(9), 2);
    assert_eq!(
        render_alpha(nested.document(), ClipState::Static),
        vec![64; 16]
    );

    // Identity groups are exactly transparent to rendering.
    let mut identity = editor(4, 4);
    add_group(
        &mut identity,
        NodeSlot::root(0),
        group(10, GroupTransform::IDENTITY, None),
    );
    add_rect(
        &mut identity,
        NodeSlot::under(id(10), 0),
        rect(1, bounds(1, 1, 2, 2), RED),
    );
    let mut bare = editor(4, 4);
    add_rect(
        &mut bare,
        NodeSlot::root(0),
        rect(1, bounds(1, 1, 2, 2), RED),
    );
    assert_eq!(
        render_pixels(identity.document(), ClipState::Static),
        render_pixels(bare.document(), ClipState::Static)
    );
}

#[test]
fn rounded_rect_mask_anti_aliases_its_edges() {
    let mut masked = editor(16, 12);
    let mask = ClipMask::rounded(bounds(2, 1, 12, 10), 4);
    add_group(
        &mut masked,
        NodeSlot::root(0),
        group(10, GroupTransform::IDENTITY, Some(mask)),
    );
    add_rect(
        &mut masked,
        NodeSlot::under(id(10), 0),
        rect(1, bounds(0, 0, 16, 12), RED),
    );
    let alpha = render_alpha(masked.document(), ClipState::Static);

    // Outside the mask rectangle nothing is painted, however close.
    for y in 0..12_usize {
        for x in 0..16_usize {
            if !(2..14).contains(&x) || !(1..11).contains(&y) {
                assert_eq!(alpha[y * 16 + x], 0, "pixel ({x}, {y})");
            }
        }
    }
    // Straight edges are pixel-aligned and fully covered, including the left
    // column away from the rounded corners.
    for y in 4..8_usize {
        assert_eq!(alpha[y * 16 + 2], 255, "row {y}");
    }
    // A rounded corner fades: at least one pixel there is partly covered and
    // never fully opaque, proving the edge is sampled rather than stepped.
    let corner = (1..5)
        .flat_map(|y| (1..5).map(move |x| (x, y)))
        .map(|(x, y)| alpha[y * 16 + x])
        .collect::<Vec<_>>();
    assert!(
        corner.iter().any(|value| *value > 0 && *value < 255),
        "{corner:?}"
    );

    // An oversized radius is limited to half the shorter side.
    let mut pill = editor(8, 4);
    let mask = ClipMask::rounded(bounds(0, 0, 8, 4), 100);
    add_group(
        &mut pill,
        NodeSlot::root(0),
        group(10, GroupTransform::IDENTITY, Some(mask)),
    );
    add_rect(
        &mut pill,
        NodeSlot::under(id(10), 0),
        rect(1, bounds(0, 0, 8, 4), RED),
    );
    let composition = pill.document().compose(ClipState::Static);
    assert_eq!(
        composition.placement(id(1)).unwrap().clips()[0].corner_radius,
        2
    );
    let alpha = render_alpha(pill.document(), ClipState::Static);
    // Columns clear of the corner arcs are fully covered on every row.
    for y in 0..4_usize {
        assert_eq!(alpha[y * 8 + 2], 255, "row {y}");
        assert_eq!(alpha[y * 8 + 5], 255, "row {y}");
    }
    // The corner pixels fade instead of snapping on.
    assert!(alpha[0] > 0 && alpha[0] < 255, "{alpha:?}");
    assert!(alpha[7] > 0 && alpha[7] < 255, "{alpha:?}");
}

#[test]
fn nested_masks_intersect_and_move_with_their_group() {
    let mut design = editor(10, 10);
    add_group(
        &mut design,
        NodeSlot::root(0),
        group(
            10,
            GroupTransform::IDENTITY,
            Some(ClipMask::rectangle(bounds(0, 0, 5, 5))),
        ),
    );
    add_group(
        &mut design,
        NodeSlot::under(id(10), 0),
        group(
            11,
            GroupTransform::offset(2, 2),
            Some(ClipMask::rectangle(bounds(1, 1, 5, 5))),
        ),
    );
    add_rect(
        &mut design,
        NodeSlot::under(id(11), 0),
        rect(1, bounds(-2, -2, 10, 10), RED),
    );
    let alpha = render_alpha(design.document(), ClipState::Static);
    for y in 0..10 {
        for x in 0..10 {
            // Outer mask 0..5, inner mask (1..6) + offset 2 = 3..8.
            let inside = (3..5).contains(&x) && (3..5).contains(&y);
            assert_eq!(alpha[y * 10 + x], if inside { 255 } else { 0 }, "({x},{y})");
        }
    }
}

#[test]
fn hidden_groups_hide_their_subtree() {
    let mut design = editor(4, 4);
    add_group(
        &mut design,
        NodeSlot::root(0),
        group(10, GroupTransform::IDENTITY, None),
    );
    add_rect(
        &mut design,
        NodeSlot::under(id(10), 0),
        rect(1, bounds(0, 0, 4, 4), RED),
    );
    design
        .apply(DesignEdit::SetGroupVisible {
            group: id(10),
            visible: false,
        })
        .unwrap();
    let composition = design.document().compose(ClipState::Static);
    assert!(composition.is_empty());
    assert_eq!(
        render_alpha(design.document(), ClipState::Static),
        vec![0; 16]
    );
}

#[test]
fn playback_state_machine_follows_frame_indices() {
    let mut playback = TitlePlayback::new(10, 5);
    assert_eq!(playback.phase(0), Ok(PlaybackPhase::Off));
    assert_eq!(playback.off_air(1), Ok(PlaybackOutcome::Ignored));
    assert_eq!(playback.take(100), Ok(PlaybackOutcome::Started(Phase::In)));
    assert_eq!(
        playback.phase(100),
        Ok(PlaybackPhase::TransitionIn { frame: 0 })
    );
    assert_eq!(
        playback.phase(109),
        Ok(PlaybackPhase::TransitionIn { frame: 9 })
    );
    assert_eq!(playback.phase(110), Ok(PlaybackPhase::Hold));
    assert_eq!(playback.phase(5_000), Ok(PlaybackPhase::Hold));
    assert_eq!(playback.take(150), Ok(PlaybackOutcome::Ignored));
    assert_eq!(playback.phase(150), Ok(PlaybackPhase::Hold));
    assert_eq!(
        playback.off_air(200),
        Ok(PlaybackOutcome::Started(Phase::Out))
    );
    assert_eq!(
        playback.phase(204),
        Ok(PlaybackPhase::TransitionOut { frame: 4 })
    );
    assert_eq!(playback.phase(205), Ok(PlaybackPhase::Off));
    assert_eq!(
        playback.phase(199),
        Err(PlaybackError::FrameRegressed {
            frame: 199,
            last_frame: 200
        })
    );
    assert_eq!(
        playback.take(199),
        Err(PlaybackError::FrameRegressed {
            frame: 199,
            last_frame: 200
        })
    );

    // Off-air during the in transition cuts to the out transition; take
    // during the out transition restarts the in transition.
    let mut playback = TitlePlayback::new(10, 5);
    playback.take(0).unwrap();
    assert_eq!(
        playback.off_air(4),
        Ok(PlaybackOutcome::Started(Phase::Out))
    );
    assert_eq!(playback.off_air(5), Ok(PlaybackOutcome::Ignored));
    assert_eq!(
        playback.phase(6),
        Ok(PlaybackPhase::TransitionOut { frame: 2 })
    );
    assert_eq!(playback.take(6), Ok(PlaybackOutcome::Started(Phase::In)));
    assert_eq!(
        playback.phase(6),
        Ok(PlaybackPhase::TransitionIn { frame: 0 })
    );

    // Zero-length transitions complete on the frame that started them.
    let mut cut = TitlePlayback::new(0, 0);
    cut.take(3).unwrap();
    assert_eq!(cut.phase(3), Ok(PlaybackPhase::Hold));
    assert_eq!(cut.sample(3), Ok(Some(ClipState::Hold)));
    cut.off_air(3).unwrap();
    assert_eq!(cut.phase(3), Ok(PlaybackPhase::Off));
    assert_eq!(cut.sample(3), Ok(None));
}

#[test]
fn transitions_animate_groups_through_take_hold_and_off_air() {
    let mut design = editor(40, 4);
    add_group(
        &mut design,
        NodeSlot::root(0),
        group(10, GroupTransform::IDENTITY, None),
    );
    add_rect(
        &mut design,
        NodeSlot::under(id(10), 0),
        rect(1, bounds(20, 0, 4, 4), RED),
    );
    for (phase, frames) in [(Phase::In, 10), (Phase::Out, 4)] {
        design
            .apply(DesignEdit::SetPhaseFrames { phase, frames })
            .unwrap();
    }
    let edits = [
        (
            track(10, Phase::In, Property::PositionX),
            keyframe(0, -20, Easing::QuadOut),
        ),
        (
            track(10, Phase::In, Property::PositionX),
            keyframe(10, 0, Easing::Linear),
        ),
        (
            track(10, Phase::Out, Property::Opacity),
            keyframe(0, 255, Easing::Linear),
        ),
        (
            track(10, Phase::Out, Property::Opacity),
            keyframe(4, 0, Easing::Linear),
        ),
    ];
    for (track, keyframe) in edits {
        design
            .apply(DesignEdit::AddKeyframe { track, keyframe })
            .unwrap();
    }
    let document = design.document();
    assert_eq!(document.animation().in_frames(), 10);
    assert_eq!(document.animation().out_frames(), 4);
    let mut playback = TitlePlayback::for_design(document);
    playback.take(1_000).unwrap();
    let offset = |frame: u64| {
        playback
            .compose(document, frame)
            .unwrap()
            .placement(id(1))
            .map(|placement| placement.offset_x)
    };
    assert_eq!(offset(1_000), Some(-20));
    // QuadOut at 0.5 is 0.75: -20 + 20 * 0.75.
    assert_eq!(offset(1_005), Some(-5));
    assert_eq!(offset(1_010), Some(0));
    assert_eq!(offset(1_500), Some(0));

    let sample = playback.sample(1_005).unwrap().unwrap();
    let alpha = render_alpha(document, sample);
    assert_eq!(alpha[15], 255);
    assert_eq!(alpha[14], 0);
    assert_eq!(alpha[19], 0);

    playback.off_air(2_000).unwrap();
    let opacity = |frame: u64| {
        playback
            .compose(document, frame)
            .unwrap()
            .placement(id(1))
            .map(|placement| placement.compose_opacity(255))
    };
    assert_eq!(opacity(2_000), Some(255));
    assert_eq!(opacity(2_002), Some(128));
    assert_eq!(opacity(2_004), None);
    assert!(playback.compose(document, 2_004).unwrap().is_empty());
}

#[test]
fn tree_edits_reorder_reparent_and_delete() {
    let mut design = editor(8, 8);
    add_group(
        &mut design,
        NodeSlot::root(0),
        group(10, GroupTransform::IDENTITY, None),
    );
    add_group(
        &mut design,
        NodeSlot::root(1),
        group(11, GroupTransform::IDENTITY, None),
    );
    add_rect(
        &mut design,
        NodeSlot::under(id(10), 0),
        rect(1, bounds(0, 0, 2, 2), RED),
    );
    add_rect(
        &mut design,
        NodeSlot::under(id(10), 1),
        rect(2, bounds(0, 0, 2, 2), RED),
    );
    add_rect(
        &mut design,
        NodeSlot::root(2),
        rect(3, bounds(0, 0, 2, 2), RED),
    );
    assert_eq!(design.document().paint_order(), vec![id(1), id(2), id(3)]);

    design
        .apply(DesignEdit::Reorder {
            node: id(2),
            index: 0,
        })
        .unwrap();
    assert_eq!(design.document().children(id(10)).unwrap(), &[id(2), id(1)]);
    design
        .apply(DesignEdit::Reparent {
            node: id(3),
            slot: NodeSlot::under(id(11), 0),
        })
        .unwrap();
    assert_eq!(design.document().parent(id(3)), Some(id(11)));
    design
        .apply(DesignEdit::Reparent {
            node: id(11),
            slot: NodeSlot::under(id(10), 1),
        })
        .unwrap();
    assert_eq!(design.document().paint_order(), vec![id(2), id(3), id(1)]);
    let template = design.document().to_template();
    let z: Vec<_> = template
        .elements
        .iter()
        .map(|e| (e.id, e.z_index))
        .collect();
    assert_eq!(z, vec![(id(2), 0), (id(3), 1), (id(1), 2)]);

    design
        .apply(DesignEdit::AddKeyframe {
            track: track(3, Phase::In, Property::PositionY),
            keyframe: keyframe(0, 1, Easing::Linear),
        })
        .unwrap();
    design
        .apply(DesignEdit::SetBounds {
            element: id(1),
            bounds: bounds(4, 4, 2, 2),
        })
        .unwrap();
    // Deleting a group removes its subtree and the tracks that target it.
    design.apply(DesignEdit::Delete { node: id(11) }).unwrap();
    let document = design.document();
    assert_eq!(document.node_count(), 3);
    assert!(document.node(id(3)).is_none());
    assert_eq!(document.animation().tracks(), &[]);
    assert_eq!(document.paint_order(), vec![id(2), id(1)]);
}

#[test]
fn keyframe_edits_add_remove_and_retime() {
    let mut design = editor(8, 8);
    add_rect(
        &mut design,
        NodeSlot::root(0),
        rect(1, bounds(0, 0, 2, 2), RED),
    );
    design
        .apply(DesignEdit::SetPhaseFrames {
            phase: Phase::In,
            frames: 20,
        })
        .unwrap();
    let key = track(1, Phase::In, Property::PositionX);
    let add = |frame: u32, value: i64| DesignEdit::AddKeyframe {
        track: key,
        keyframe: keyframe(frame, value, Easing::Linear),
    };
    design.apply(add(10, 100)).unwrap();
    design.apply(add(0, 0)).unwrap();
    assert_eq!(keys(&design, key), vec![(0, 0), (10, 100)]);
    let sample = |design: &DesignEditor, frame: u32| {
        design
            .document()
            .animation()
            .sample(id(1), Property::PositionX, in_frame(frame))
    };
    assert_eq!(sample(&design, 5), Some(Value::Scalar(50)));
    assert_eq!(sample(&design, 15), Some(Value::Scalar(100)));
    assert_eq!(
        design.apply(add(10, 7)),
        Err(DesignError::KeyframeExists {
            track: key,
            frame: 10
        })
    );

    design
        .apply(DesignEdit::RetimeKeyframe {
            track: key,
            from: 10,
            to: 20,
        })
        .unwrap();
    assert_eq!(sample(&design, 10), Some(Value::Scalar(50)));
    // Landing on another keyframe is refused.
    assert_eq!(
        design.apply(DesignEdit::RetimeKeyframe {
            track: key,
            from: 0,
            to: 20,
        }),
        Err(DesignError::KeyframeExists {
            track: key,
            frame: 20
        })
    );
    design
        .apply(DesignEdit::RetimeKeyframe {
            track: key,
            from: 20,
            to: 5,
        })
        .unwrap();
    assert_eq!(keys(&design, key), vec![(0, 0), (5, 100)]);

    // Stretching the phase scales every key proportionally.
    design
        .apply(DesignEdit::StretchPhase {
            phase: Phase::In,
            frames: 40,
        })
        .unwrap();
    assert_eq!(design.document().animation().in_frames(), 40);
    assert_eq!(keys(&design, key), vec![(0, 0), (10, 100)]);

    for frame in [0, 10] {
        design
            .apply(DesignEdit::RemoveKeyframe { track: key, frame })
            .unwrap();
    }
    assert_eq!(design.document().animation().tracks(), &[]);
}

#[test]
#[allow(clippy::too_many_lines)]
fn invalid_edits_change_nothing() {
    let mut design = editor(8, 8);
    add_rect(
        &mut design,
        NodeSlot::root(0),
        rect(1, bounds(0, 0, 2, 2), RED),
    );
    design
        .apply(DesignEdit::SetPhaseFrames {
            phase: Phase::Out,
            frames: 5,
        })
        .unwrap();
    let mut parent = None;
    for level in 0..MAX_GROUP_DEPTH {
        let group_id = 100 + u128::try_from(level).unwrap();
        add_group(
            &mut design,
            NodeSlot { parent, index: 0 },
            group(group_id, GroupTransform::IDENTITY, None),
        );
        parent = Some(id(group_id));
    }
    let snapshot = design.document().clone();
    let revision = design.revision();
    let undo = design.undo_len();

    let deepest = parent.unwrap();
    let refused = [
        (
            DesignEdit::AddGroup {
                slot: NodeSlot::under(deepest, 0),
                group: group(50, GroupTransform::IDENTITY, None),
            },
            DesignError::GroupTooDeep {
                group: id(50),
                maximum: MAX_GROUP_DEPTH,
            },
        ),
        (
            DesignEdit::AddElement {
                slot: NodeSlot::root(0),
                element: rect(1, bounds(0, 0, 1, 1), RED),
            },
            DesignError::DuplicateNode(id(1)),
        ),
        (
            DesignEdit::AddElement {
                slot: NodeSlot::under(id(1), 0),
                element: rect(2, bounds(0, 0, 1, 1), RED),
            },
            DesignError::NotAGroup(id(1)),
        ),
        (
            DesignEdit::AddElement {
                slot: NodeSlot::root(9),
                element: rect(2, bounds(0, 0, 1, 1), RED),
            },
            DesignError::IndexOutOfRange {
                index: 9,
                length: 2,
            },
        ),
        (
            DesignEdit::Reparent {
                node: id(100),
                slot: NodeSlot::under(id(103), 0),
            },
            DesignError::ReparentIntoSelf(id(100)),
        ),
        (
            DesignEdit::SetBounds {
                element: id(1),
                bounds: bounds(0, 0, 0, 2),
            },
            DesignError::Template(vec![fm_titles::ValidationError::ZeroElementBounds(id(1))]),
        ),
        (
            DesignEdit::SetGroupMask {
                group: id(100),
                mask: Some(ClipMask::rectangle(bounds(0, 0, 0, 1))),
            },
            DesignError::EmptyMask(id(100)),
        ),
        (
            DesignEdit::AddKeyframe {
                track: track(1, Phase::Out, Property::Opacity),
                keyframe: keyframe(0, 300, Easing::Linear),
            },
            DesignError::Animation(ClipError::InvalidValue {
                track: track(1, Phase::Out, Property::Opacity),
                frame: 0,
            }),
        ),
        (
            DesignEdit::AddKeyframe {
                track: track(1, Phase::Out, Property::PositionX),
                keyframe: keyframe(6, 0, Easing::Linear),
            },
            DesignError::Animation(ClipError::KeyOutsidePhase {
                track: track(1, Phase::Out, Property::PositionX),
                frame: 6,
            }),
        ),
        (
            DesignEdit::AddKeyframe {
                track: track(100, Phase::In, Property::ScaleX),
                keyframe: keyframe(0, 0, Easing::Linear),
            },
            DesignError::UnsupportedGroupProperty {
                node: id(100),
                property: Property::ScaleX,
            },
        ),
        (
            DesignEdit::AddKeyframe {
                track: track(77, Phase::In, Property::PositionX),
                keyframe: keyframe(0, 0, Easing::Linear),
            },
            DesignError::UnknownNode(id(77)),
        ),
        (
            DesignEdit::RemoveKeyframe {
                track: track(1, Phase::Out, Property::PositionX),
                frame: 0,
            },
            DesignError::UnknownKeyframe {
                track: track(1, Phase::Out, Property::PositionX),
                frame: 0,
            },
        ),
        (
            DesignEdit::Delete { node: id(77) },
            DesignError::UnknownNode(id(77)),
        ),
    ];
    for (edit, error) in refused {
        assert_eq!(design.apply(edit.clone()), Err(error), "{edit:?}");
        assert_eq!(design.document(), &snapshot, "{edit:?}");
        assert_eq!(design.revision(), revision);
        assert_eq!(design.undo_len(), undo);
    }

    // A keyframe left past a shortened phase refuses the shortening.
    design
        .apply(DesignEdit::AddKeyframe {
            track: track(1, Phase::Out, Property::PositionX),
            keyframe: keyframe(5, 0, Easing::Linear),
        })
        .unwrap();
    assert!(matches!(
        design.apply(DesignEdit::SetPhaseFrames {
            phase: Phase::Out,
            frames: 4,
        }),
        Err(DesignError::Animation(ClipError::KeyOutsidePhase {
            frame: 5,
            ..
        }))
    ));
}

#[test]
fn undo_redo_restores_exact_documents_with_bounded_depth() {
    let base = editor(8, 8).document().clone();
    let mut design = DesignEditor::with_undo_depth(base.clone(), 3).unwrap();
    let mut history = vec![design.document().clone()];
    for element in 1..=5 {
        add_rect(
            &mut design,
            NodeSlot::root(0),
            rect(element, bounds(0, 0, 1, 1), RED),
        );
        history.push(design.document().clone());
    }
    assert_eq!(design.undo_len(), 3);
    for expected in history[2..5].iter().rev() {
        design.undo().unwrap();
        assert_eq!(design.document(), expected);
    }
    assert_eq!(design.undo(), Err(DesignError::NothingToUndo));
    assert_eq!(design.document(), &history[2]);
    assert_eq!(design.redo_len(), 3);

    design.redo().unwrap();
    assert_eq!(design.document(), &history[3]);
    design.redo().unwrap();
    assert_eq!(design.document(), &history[4]);

    // A new edit discards the redo branch.
    design.apply(DesignEdit::Delete { node: id(1) }).unwrap();
    assert_eq!(design.redo_len(), 0);
    assert_eq!(design.redo(), Err(DesignError::NothingToRedo));
    design.undo().unwrap();
    assert_eq!(design.document(), &history[4]);

    let revision = design.revision();
    assert_eq!(revision, 5 + 3 + 2 + 1 + 1);

    // Zero depth keeps no history.
    let mut none = DesignEditor::with_undo_depth(base, 0).unwrap();
    add_rect(
        &mut none,
        NodeSlot::root(0),
        rect(1, bounds(0, 0, 1, 1), RED),
    );
    assert_eq!(none.undo(), Err(DesignError::NothingToUndo));
}

#[test]
fn overshooting_opacity_easing_clamps_to_opaque_instead_of_vanishing() {
    let mut design = editor(8, 8);
    add_group(
        &mut design,
        NodeSlot::root(0),
        group(10, GroupTransform::IDENTITY, None),
    );
    add_rect(
        &mut design,
        NodeSlot::under(id(10), 0),
        rect(1, bounds(0, 0, 2, 2), RED),
    );
    design
        .apply(DesignEdit::SetPhaseFrames {
            phase: Phase::In,
            frames: 10,
        })
        .unwrap();
    // easeOutBack-style control points overshoot past the end value.
    let back = Easing::CubicBezier(CubicBezier::new(22_282, 102_236, 41_943, 65_536).unwrap());
    let key = track(10, Phase::In, Property::Opacity);
    for frame in [keyframe(0, 0, back), keyframe(10, 255, Easing::Linear)] {
        design
            .apply(DesignEdit::AddKeyframe {
                track: key,
                keyframe: frame,
            })
            .unwrap();
    }
    let document = design.document();
    let raw = document.animation().track(key).unwrap();
    let mut overshot = false;
    for frame in 0..=10 {
        let Some(Value::Scalar(value)) = raw.sample(ClipPosition::frame(frame)) else {
            panic!("scalar opacity");
        };
        overshot |= value > 255;
        let opacity = document
            .compose(in_frame(frame))
            .placement(id(1))
            .unwrap()
            .compose_opacity(255);
        assert_eq!(i64::from(opacity), value.clamp(0, 255), "frame {frame}");
    }
    assert!(overshot, "the easing must overshoot for this test to bite");
}

#[test]
fn element_tracks_travel_with_the_template_and_group_tracks_stay_in_the_design() {
    let mut design = editor(8, 2);
    add_group(
        &mut design,
        NodeSlot::root(0),
        group(10, GroupTransform::IDENTITY, None),
    );
    add_rect(
        &mut design,
        NodeSlot::under(id(10), 0),
        rect(1, bounds(0, 0, 2, 2), RED),
    );
    design
        .apply(DesignEdit::SetPhaseFrames {
            phase: Phase::In,
            frames: 4,
        })
        .unwrap();
    for (key, keyframe) in [
        (
            track(10, Phase::In, Property::PositionX),
            keyframe(0, 2, Easing::Linear),
        ),
        (
            track(1, Phase::In, Property::PositionX),
            keyframe(0, 4, Easing::Linear),
        ),
        (
            track(1, Phase::In, Property::PositionX),
            keyframe(4, 0, Easing::Linear),
        ),
    ] {
        design
            .apply(DesignEdit::AddKeyframe {
                track: key,
                keyframe,
            })
            .unwrap();
    }
    let template = design.document().to_template();
    assert_eq!(template.animation.clip.in_frames(), 4);
    assert_eq!(
        template
            .animation
            .clip
            .tracks()
            .iter()
            .map(|track| track.key)
            .collect::<Vec<_>>(),
        vec![track(1, Phase::In, Property::PositionX)]
    );
    // Frame 0: group +2, element +4 -> columns 6 and 7. Hold: group +2 only.
    let alpha = render_alpha(design.document(), in_frame(0));
    assert_eq!(&alpha[..8], &[0, 0, 0, 0, 0, 0, 255, 255]);
    let alpha = render_alpha(design.document(), ClipState::Hold);
    assert_eq!(&alpha[..8], &[0, 0, 255, 255, 0, 0, 0, 0]);
}
