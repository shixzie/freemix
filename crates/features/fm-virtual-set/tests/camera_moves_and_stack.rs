//! Exact checks for virtual-camera moves, layer-stack frame plans with
//! reflection/shadow hooks, and frame-boundary preset recall.

use std::num::NonZeroU128;

use fm_types::InputId;
use fm_virtual_set::{
    CameraDirector, CameraMove, CameraPose, EasingCurve, EasingPoint, FramePlan, KeyReference,
    LayerId, LayerStack, MAX_EASING_POINTS, MAX_PRESET_NAME_BYTES, MAX_PRESETS, MAX_SHADOW_BLUR_PX,
    MAX_STACK_LAYERS, MAX_TALENT_SLOTS, OutputSize, PixelBounds, PixelRect, PlanError,
    PlannedElement, PresetId, PresetStore, Rational, ReflectionHook, SetRect, ShadowHook,
    StackLayer, StackRole, TalentId, VirtualSetPreset, plan_frame,
};

fn nz(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn layer(value: u128) -> LayerId {
    LayerId::new(nz(value))
}

fn talent(value: u128) -> TalentId {
    TalentId::new(nz(value))
}

fn input(value: u128) -> InputId {
    InputId::new(nz(value))
}

fn preset_id(value: u128) -> PresetId {
    PresetId::new(nz(value))
}

fn r(numer: i128, denom: i128) -> Rational {
    Rational::new(numer, denom).unwrap()
}

fn pose(x: u32, y: u32, zoom: u32) -> CameraPose {
    CameraPose::new(x, y, zoom).unwrap()
}

fn assert_state(
    state: fm_virtual_set::CameraState,
    pan_x: Rational,
    pan_y: Rational,
    zoom: Rational,
) {
    assert_eq!(
        (state.pan_x(), state.pan_y(), state.zoom()),
        (pan_x, pan_y, zoom)
    );
}

#[test]
fn rational_arithmetic_is_exact_and_rounds_half_up() {
    let value = r(2, -4);
    assert_eq!((value.numer(), value.denom()), (-1, 2));
    assert_eq!(value.floor(), -1);
    assert_eq!(value.round_half_up(), 0);
    assert_eq!(r(5, 2).round_half_up(), 3);
    assert_eq!(r(1, 3).quantize(1_000_000).unwrap(), r(333_333, 1_000_000));
    assert_eq!(r(1, 3).checked_add(r(1, 6)).unwrap(), r(1, 2));
    assert_eq!(r(1, 3).checked_div(r(2, 9)).unwrap(), r(3, 2));
    assert_eq!(r(7, 3).to_string(), "7/3");
    assert_eq!(Rational::new(1, 0), Err(PlanError::ZeroDenominator));
    assert_eq!(
        Rational::ONE.checked_div(Rational::ZERO),
        Err(PlanError::ZeroDenominator)
    );
}

#[test]
fn camera_pose_and_easing_validation_is_explicit() {
    assert_eq!(
        CameraPose::new(1001, 0, 1000),
        Err(PlanError::PanOutOfRange {
            value_permille: 1001
        })
    );
    assert_eq!(
        CameraPose::new(0, 0, 999),
        Err(PlanError::ZoomOutOfRange {
            value_permille: 999,
            minimum: 1000,
            maximum: 8000
        })
    );
    assert!(CameraPose::new(0, 1000, 8000).is_ok());

    assert_eq!(
        EasingCurve::new(vec![EasingPoint::new(0, 0)]),
        Err(PlanError::TooFewEasingPoints { actual: 1 })
    );
    assert_eq!(
        EasingCurve::new(vec![EasingPoint::new(0, 0), EasingPoint::new(1000, 999)]),
        Err(PlanError::EasingEndpoints)
    );
    assert_eq!(
        EasingCurve::new(vec![
            EasingPoint::new(0, 0),
            EasingPoint::new(500, 200),
            EasingPoint::new(500, 300),
            EasingPoint::new(1000, 1000),
        ]),
        Err(PlanError::EasingNotIncreasing { index: 2 })
    );
    assert_eq!(
        EasingCurve::new(vec![
            EasingPoint::new(0, 0),
            EasingPoint::new(500, 1001),
            EasingPoint::new(1000, 1000),
        ]),
        Err(PlanError::EasingValueOutOfRange { index: 1 })
    );
    let too_many = (0..=u32::try_from(MAX_EASING_POINTS).unwrap())
        .map(|index| EasingPoint::new(index * 50, index * 50))
        .collect::<Vec<_>>();
    assert_eq!(
        EasingCurve::new(too_many),
        Err(PlanError::TooManyEasingPoints {
            actual: MAX_EASING_POINTS + 1,
            maximum: MAX_EASING_POINTS
        })
    );
}

#[test]
fn easing_curves_interpolate_integer_control_points_exactly() {
    let curve = EasingCurve::ease_in_out();
    assert_eq!(curve.ease(r(1, 8)).unwrap(), r(1, 20));
    assert_eq!(curve.ease(r(3, 8)).unwrap(), r(3, 10));
    assert_eq!(curve.ease(r(1, 3)).unwrap(), r(7, 30));
    assert_eq!(curve.ease(r(1, 2)).unwrap(), r(1, 2));
    assert_eq!(curve.ease(r(-1, 2)).unwrap(), Rational::ZERO);
    assert_eq!(curve.ease(r(3, 2)).unwrap(), Rational::ONE);
    assert_eq!(EasingCurve::linear().ease(r(2, 7)).unwrap(), r(2, 7));
}

#[test]
fn preset_to_preset_moves_interpolate_per_frame() {
    let from = CameraPose::wide().state();
    let to = pose(250, 400, 2000).state();
    let linear = CameraMove::new(from, to, 10, 4, EasingCurve::linear()).unwrap();
    assert_eq!(linear.state_at(9).unwrap(), from);
    assert_eq!(linear.state_at(10).unwrap(), from);
    assert_state(linear.state_at(11).unwrap(), r(7, 16), r(19, 40), r(5, 4));
    assert_state(linear.state_at(12).unwrap(), r(3, 8), r(9, 20), r(3, 2));
    assert_eq!(linear.state_at(14).unwrap(), to);
    assert_eq!(linear.state_at(1_000).unwrap(), to);
    assert!(!linear.is_complete_at(13));
    assert!(linear.is_complete_at(14));

    let eased = CameraMove::new(from, to, 0, 8, EasingCurve::ease_in_out()).unwrap();
    assert_state(eased.state_at(1).unwrap(), r(39, 80), r(99, 200), r(21, 20));
    assert_state(eased.state_at(4).unwrap(), r(3, 8), r(9, 20), r(3, 2));

    let cut = CameraMove::new(from, to, 5, 0, EasingCurve::linear()).unwrap();
    assert_eq!(cut.state_at(4).unwrap(), from);
    assert_eq!(cut.state_at(5).unwrap(), to);

    assert_eq!(
        CameraMove::new(from, to, 0, 36_001, EasingCurve::linear()),
        Err(PlanError::MoveTooLong {
            frames: 36_001,
            maximum: 36_000
        })
    );
}

#[test]
fn camera_window_is_clamped_inside_the_set() {
    let clamped = pose(0, 1000, 4000).state().clamped().unwrap();
    assert_state(clamped, r(1, 8), r(7, 8), r(4, 1));
    let wide = CameraPose::wide().state().clamped().unwrap();
    assert_state(wide, r(1, 2), r(1, 2), Rational::ONE);
}

fn studio_stack() -> LayerStack {
    let presenter = StackLayer::new(
        layer(2),
        StackRole::Talent {
            talent_id: talent(1),
            key: KeyReference::ExternalMatte(input(21)),
        },
        input(20),
        0,
        SetRect::new(300, 200, 500, 800),
    )
    .with_reflection(ReflectionHook {
        axis_y_permille: 850,
        opacity_permille: 400,
        fade_permille: 500,
    })
    .with_shadow(ShadowHook {
        offset_x_permille: 20,
        offset_y_permille: 10,
        blur_radius_px: 6,
        opacity_permille: 500,
    });
    let desk = StackLayer::new(
        layer(3),
        StackRole::Foreground,
        input(30),
        -100,
        SetRect::new(0, 750, 1000, 1000),
    )
    .with_parallax(500)
    .with_opacity(900);
    let set = StackLayer::new(
        layer(1),
        StackRole::Background,
        input(10),
        100,
        SetRect::full(),
    );
    // Authored out of order: bands, not raw z-order, decide draw order.
    LayerStack::new(vec![desk, presenter, set]).unwrap()
}

fn studio_plan() -> FramePlan {
    let output = OutputSize::new(1920, 1080).unwrap();
    plan_frame(&studio_stack(), pose(250, 400, 2000).state(), output, 42).unwrap()
}

#[test]
fn frame_plan_projects_layers_and_shadow_into_output_pixels() {
    let plan = studio_plan();
    assert_eq!(plan.frame, 42);
    assert_eq!(plan.layers.len(), 5);

    let ids = plan.layers.iter().map(|l| l.layer_id).collect::<Vec<_>>();
    assert_eq!(ids, [layer(1), layer(2), layer(2), layer(2), layer(3)]);

    let background = plan.layers[0];
    assert_eq!(background.element, PlannedElement::Background);
    assert_eq!(
        background.rect,
        PixelRect {
            left: r(0, 1),
            top: r(-324, 1),
            right: r(3840, 1),
            bottom: r(1836, 1),
        }
    );

    let matte = Some(KeyReference::ExternalMatte(input(21)));
    let shadow = plan.layers[1];
    assert_eq!(
        shadow.element,
        PlannedElement::Shadow {
            matte,
            blur_radius_px: r(12, 1)
        }
    );
    assert_eq!(
        shadow.rect,
        PixelRect {
            left: r(6144, 5),
            top: r(648, 5),
            right: r(9984, 5),
            bottom: r(7128, 5),
        }
    );
    assert_eq!(
        shadow.rect.rounded(),
        PixelBounds {
            left: 1229,
            top: 130,
            right: 1997,
            bottom: 1426
        }
    );
    assert_eq!(shadow.opacity, r(1, 2));
    assert!(!shadow.flip_vertical);

    // Identical inputs always produce identical plans.
    assert_eq!(plan, studio_plan());
}

#[test]
fn frame_plan_derives_reflection_talent_and_parallax_desk() {
    let plan = studio_plan();
    let matte = Some(KeyReference::ExternalMatte(input(21)));
    let reflection = plan.layers[2];
    assert_eq!(
        reflection.element,
        PlannedElement::Reflection {
            matte,
            fade_start_y: r(1620, 1),
            fade_end_y: r(2268, 1),
        }
    );
    assert_eq!(
        reflection.rect,
        PixelRect {
            left: r(1152, 1),
            top: r(1620, 1),
            right: r(1920, 1),
            bottom: r(2916, 1),
        }
    );
    assert_eq!(reflection.opacity, r(2, 5));
    assert!(reflection.flip_vertical);

    let presenter = plan.layers[3];
    assert_eq!(
        presenter.element,
        PlannedElement::Talent {
            talent_id: talent(1),
            key: KeyReference::ExternalMatte(input(21)),
        }
    );
    assert_eq!(presenter.source, input(20));
    assert_eq!(
        presenter.rect,
        PixelRect {
            left: r(1152, 1),
            top: r(108, 1),
            right: r(1920, 1),
            bottom: r(1404, 1),
        }
    );
    assert_eq!(presenter.opacity, Rational::ONE);

    // Half parallax: effective zoom 3/2 centred on (3/8, 9/20).
    let desk = plan.layers[4];
    assert_eq!(desk.element, PlannedElement::Foreground);
    assert_eq!(
        desk.rect,
        PixelRect {
            left: r(-120, 1),
            top: r(1026, 1),
            right: r(2760, 1),
            bottom: r(1431, 1),
        }
    );
    assert_eq!(desk.opacity, r(9, 10));
}

#[test]
fn wide_shot_maps_set_space_one_to_one() {
    let plan = plan_frame(
        &studio_stack(),
        CameraPose::wide().state(),
        OutputSize::new(1920, 1080).unwrap(),
        0,
    )
    .unwrap();
    assert_eq!(
        plan.layers[3].rect.rounded(),
        PixelBounds {
            left: 576,
            top: 216,
            right: 960,
            bottom: 864
        }
    );
    assert_eq!(
        plan.layers[1],
        fm_virtual_set::PlannedLayer {
            element: PlannedElement::Shadow {
                matte: Some(KeyReference::ExternalMatte(input(21))),
                blur_radius_px: r(6, 1),
            },
            ..plan.layers[1]
        }
    );
}

fn background() -> StackLayer {
    StackLayer::new(
        layer(1),
        StackRole::Background,
        input(1),
        0,
        SetRect::full(),
    )
}

fn fg(id: u128, z: i32) -> StackLayer {
    StackLayer::new(
        layer(id),
        StackRole::Foreground,
        input(2),
        z,
        SetRect::new(0, 800, 1000, 1000),
    )
}

fn slot(id: u128, talent_value: u128, z: i32) -> StackLayer {
    StackLayer::new(
        layer(id),
        StackRole::Talent {
            talent_id: talent(talent_value),
            key: KeyReference::FillAlpha,
        },
        input(3),
        z,
        SetRect::new(300, 200, 500, 800),
    )
}

#[test]
fn layer_stack_validation_rejects_bad_structure() {
    let background = background();

    assert_eq!(LayerStack::new(vec![]), Err(PlanError::EmptyStack));
    assert_eq!(
        LayerStack::new(vec![fg(2, 0)]),
        Err(PlanError::MissingBackground)
    );
    assert_eq!(
        LayerStack::new(vec![background, fg(1, 1)]),
        Err(PlanError::DuplicateStackLayer(layer(1)))
    );
    assert_eq!(
        LayerStack::new(vec![background, slot(2, 7, 0), slot(3, 7, 1)]),
        Err(PlanError::DuplicateTalentSlot(talent(7)))
    );
    assert_eq!(
        LayerStack::new(vec![background, fg(2, 5), fg(3, 5)]),
        Err(PlanError::DuplicateBandZOrder {
            z_order: 5,
            first: layer(2),
            second: layer(3)
        })
    );
    // The same z-order in different bands is allowed.
    assert!(LayerStack::new(vec![background, fg(2, 0), slot(3, 1, 0)]).is_ok());
}

#[test]
fn layer_stack_validation_rejects_bad_hooks_and_bounds() {
    let background = background();
    let shadow = ShadowHook {
        offset_x_permille: 0,
        offset_y_permille: 0,
        blur_radius_px: 4,
        opacity_permille: 500,
    };
    assert_eq!(
        LayerStack::new(vec![background.with_shadow(shadow)]),
        Err(PlanError::HookOnBackground(layer(1)))
    );
    assert_eq!(
        LayerStack::new(vec![
            background,
            slot(2, 1, 0).with_shadow(ShadowHook {
                blur_radius_px: MAX_SHADOW_BLUR_PX + 1,
                ..shadow
            })
        ]),
        Err(PlanError::ShadowBlurTooLarge {
            layer_id: layer(2),
            radius_px: 65,
            maximum: 64
        })
    );
    assert_eq!(
        LayerStack::new(vec![
            background,
            slot(2, 1, 0).with_shadow(ShadowHook {
                offset_x_permille: -251,
                ..shadow
            })
        ]),
        Err(PlanError::ShadowOffsetOutOfRange(layer(2)))
    );
    let reflection = ReflectionHook {
        axis_y_permille: 799,
        opacity_permille: 300,
        fade_permille: 500,
    };
    assert_eq!(
        LayerStack::new(vec![background, slot(2, 1, 0).with_reflection(reflection)]),
        Err(PlanError::ReflectionAxisInsideLayer(layer(2)))
    );
    assert_eq!(
        LayerStack::new(vec![
            background,
            slot(2, 1, 0).with_reflection(ReflectionHook {
                axis_y_permille: 800,
                fade_permille: 0,
                ..reflection
            })
        ]),
        Err(PlanError::InvalidReflectionFade(layer(2)))
    );
    let mut inverted = fg(2, 0);
    inverted.rect = SetRect::new(500, 0, 500, 100);
    assert_eq!(
        LayerStack::new(vec![background, inverted]),
        Err(PlanError::InvalidSetRect(layer(2)))
    );
    assert_eq!(
        LayerStack::new(vec![background, fg(2, 0).with_parallax(1001)]),
        Err(PlanError::ParallaxOutOfRange(layer(2)))
    );
    assert_eq!(
        LayerStack::new(vec![background, fg(2, 0).with_opacity(1001)]),
        Err(PlanError::OpacityOutOfRange(layer(2)))
    );
}

#[test]
fn layer_stack_and_output_sizes_are_bounded() {
    let background = background();
    let mut crowded = vec![background];
    crowded.extend(
        (0..u128::try_from(MAX_STACK_LAYERS).unwrap())
            .map(|i| fg(10 + i, i32::try_from(i).unwrap())),
    );
    assert_eq!(
        LayerStack::new(crowded),
        Err(PlanError::TooManyStackLayers {
            actual: 33,
            maximum: 32
        })
    );
    let mut talents = vec![background];
    talents.extend(
        (0..=u128::try_from(MAX_TALENT_SLOTS).unwrap())
            .map(|i| slot(10 + i, 1 + i, i32::try_from(i).unwrap())),
    );
    assert_eq!(
        LayerStack::new(talents),
        Err(PlanError::TooManyTalentSlots {
            actual: 9,
            maximum: 8
        })
    );

    assert_eq!(
        OutputSize::new(0, 1080),
        Err(PlanError::InvalidOutputSize {
            width: 0,
            height: 1080
        })
    );
    assert_eq!(
        OutputSize::new(1920, 16_385),
        Err(PlanError::InvalidOutputSize {
            width: 1920,
            height: 16_385
        })
    );
}

fn preset(id: u128, name: &str, camera: CameraPose, frames: u32) -> VirtualSetPreset {
    VirtualSetPreset::new(preset_id(id), name, camera, frames, EasingCurve::linear()).unwrap()
}

#[test]
fn preset_store_validates_and_bounds_presets() {
    assert_eq!(
        VirtualSetPreset::new(
            preset_id(1),
            "  ",
            CameraPose::wide(),
            0,
            EasingCurve::linear()
        ),
        Err(PlanError::EmptyPresetName)
    );
    assert_eq!(
        VirtualSetPreset::new(
            preset_id(1),
            "x".repeat(MAX_PRESET_NAME_BYTES + 1),
            CameraPose::wide(),
            0,
            EasingCurve::linear()
        ),
        Err(PlanError::PresetNameTooLong {
            bytes: 65,
            maximum: 64
        })
    );
    assert_eq!(
        VirtualSetPreset::new(
            preset_id(1),
            "Wide",
            CameraPose::wide(),
            36_001,
            EasingCurve::linear()
        ),
        Err(PlanError::MoveTooLong {
            frames: 36_001,
            maximum: 36_000
        })
    );

    let mut store = PresetStore::new();
    store
        .insert(preset(1, "Wide", CameraPose::wide(), 0))
        .unwrap();
    assert_eq!(
        store.insert(preset(1, "Again", CameraPose::wide(), 0)),
        Err(PlanError::DuplicatePreset(preset_id(1)))
    );
    for id in 2..=u128::try_from(MAX_PRESETS).unwrap() {
        store
            .insert(preset(id, "Fill", CameraPose::wide(), 0))
            .unwrap();
    }
    assert_eq!(store.len(), MAX_PRESETS);
    assert_eq!(
        store.insert(preset(1_000, "Overflow", CameraPose::wide(), 0)),
        Err(PlanError::TooManyPresets { maximum: 64 })
    );
    let previous = store
        .replace(preset(1, "Wide 2", pose(500, 500, 1500), 10))
        .unwrap();
    assert_eq!(previous.name(), "Wide");
    assert_eq!(
        store.get(preset_id(1)).unwrap().pose(),
        pose(500, 500, 1500)
    );
    assert_eq!(
        store.replace(preset(1_000, "Missing", CameraPose::wide(), 0)),
        Err(PlanError::UnknownPreset(preset_id(1_000)))
    );
    assert_eq!(store.remove(preset_id(2)).unwrap().id(), preset_id(2));
    assert_eq!(
        store.remove(preset_id(2)),
        Err(PlanError::UnknownPreset(preset_id(2)))
    );
    assert_eq!(store.iter().next().unwrap().id(), preset_id(1));
}

fn recall_store() -> PresetStore {
    let mut store = PresetStore::new();
    store
        .insert(preset(1, "Two-shot", pose(250, 400, 2000), 4))
        .unwrap();
    store
        .insert(preset(2, "Desk", pose(750, 500, 1000), 2))
        .unwrap();
    store
}

#[test]
fn recall_applies_atomically_at_the_frame_boundary() {
    let store = recall_store();
    let mut director = CameraDirector::new(CameraPose::wide(), 0);
    assert_eq!(
        director.request_recall(&store, preset_id(1), 10).unwrap(),
        None
    );

    let wide = CameraPose::wide().state();
    for frame in 0..10 {
        assert_eq!(director.state_at(frame).unwrap(), wide);
        assert_eq!(director.active_preset(), None);
    }
    assert_eq!(director.state_at(10).unwrap(), wide);
    assert_eq!(director.active_preset(), Some(preset_id(1)));
    assert!(director.pending().is_none());
    assert_state(director.state_at(11).unwrap(), r(7, 16), r(19, 40), r(5, 4));

    // Interrupt mid-move: the new move starts from the exact frame-12 state.
    director.request_recall(&store, preset_id(2), 12).unwrap();
    assert_state(director.state_at(12).unwrap(), r(3, 8), r(9, 20), r(3, 2));
    assert_state(director.state_at(13).unwrap(), r(9, 16), r(19, 40), r(5, 4));
    assert_eq!(director.state_at(14).unwrap(), pose(750, 500, 1000).state());
    assert_eq!(director.active_preset(), Some(preset_id(2)));

    // A sparse evaluator lands on the same timeline as a per-frame one.
    let mut sparse = CameraDirector::new(CameraPose::wide(), 0);
    sparse.request_recall(&store, preset_id(1), 10).unwrap();
    assert_state(sparse.state_at(11).unwrap(), r(7, 16), r(19, 40), r(5, 4));
    sparse.request_recall(&store, preset_id(2), 12).unwrap();
    assert_state(sparse.state_at(13).unwrap(), r(9, 16), r(19, 40), r(5, 4));
}

#[test]
fn recall_snapshots_presets_and_rejects_bad_requests_without_side_effects() {
    let mut store = recall_store();
    let mut director = CameraDirector::new(CameraPose::wide(), 0);
    director.state_at(5).unwrap();

    assert_eq!(
        director.request_recall(&store, preset_id(9), 6),
        Err(PlanError::UnknownPreset(preset_id(9)))
    );
    assert_eq!(
        director.request_recall(&store, preset_id(1), 5),
        Err(PlanError::RecallInPast {
            requested: 5,
            earliest: 6
        })
    );
    assert!(director.pending().is_none());
    assert_eq!(
        director.state_at(4),
        Err(PlanError::NonMonotonicFrame {
            requested: 4,
            last: 5
        })
    );

    director.request_recall(&store, preset_id(1), 8).unwrap();
    let replaced = director
        .request_recall(&store, preset_id(2), 9)
        .unwrap()
        .unwrap();
    assert_eq!((replaced.at_frame, replaced.preset.id()), (8, preset_id(1)));

    // Editing or deleting the stored preset after the request cannot tear the recall.
    store
        .replace(preset(2, "Desk", pose(0, 0, 8000), 0))
        .unwrap();
    store.remove(preset_id(2)).unwrap();
    assert_eq!(director.state_at(8).unwrap(), CameraPose::wide().state());
    assert_eq!(director.state_at(11).unwrap(), pose(750, 500, 1000).state());

    director
        .request_recall(&recall_store(), preset_id(1), 20)
        .unwrap();
    assert_eq!(director.cancel_recall().unwrap().at_frame, 20);
    assert_eq!(director.state_at(30).unwrap(), pose(750, 500, 1000).state());
}

#[test]
fn interrupted_eased_move_restarts_from_quantized_state() {
    let mut store = PresetStore::new();
    store
        .insert(
            VirtualSetPreset::new(
                preset_id(1),
                "Push",
                pose(250, 400, 2000),
                3,
                EasingCurve::ease_in_out(),
            )
            .unwrap(),
        )
        .unwrap();
    store
        .insert(preset(2, "Wide cut", CameraPose::wide(), 0))
        .unwrap();

    let mut director = CameraDirector::new(CameraPose::wide(), 0);
    director.request_recall(&store, preset_id(1), 0).unwrap();
    director.state_at(0).unwrap();
    // ease(1/3) = 7/30 exactly before quantization.
    assert_state(
        director.active_move().state_at(1).unwrap(),
        r(53, 120),
        r(143, 300),
        r(37, 30),
    );
    director.request_recall(&store, preset_id(2), 1).unwrap();
    assert_eq!(director.state_at(1).unwrap(), CameraPose::wide().state());
    let start = director.active_move().from();
    assert_state(
        start,
        r(441_667, 1_000_000),
        r(476_667, 1_000_000),
        r(1_233_333, 1_000_000),
    );
}
