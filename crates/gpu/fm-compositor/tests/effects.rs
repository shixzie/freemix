use fm_compositor::{
    CompositionPlan, CpuExecutionError, CpuSourceFrame, Effect, OutputTarget, ReferenceEffect,
    ReferenceEffectError, Rgba8, Rotation, Scene, SourceId, SourceLayer, Transform,
    apply_reference_effect, compile_scene, execute_cpu,
};
use fm_video::ImageFrame;

fn frame(width: u32, height: u32, pixels: &[Rgba8]) -> ImageFrame {
    ImageFrame::new(
        width,
        height,
        usize::try_from(width).unwrap() * 4,
        pixels.iter().flat_map(|pixel| pixel.to_bytes()).collect(),
    )
    .unwrap()
}

fn padded_frame(width: u32, height: u32, pixels: &[Rgba8], padding: usize) -> ImageFrame {
    let stride = usize::try_from(width).unwrap() * 4 + padding;
    let mut bytes = Vec::with_capacity(stride * usize::try_from(height).unwrap());
    for row in pixels.chunks_exact(usize::try_from(width).unwrap()) {
        for pixel in row {
            bytes.extend_from_slice(&pixel.to_bytes());
        }
        bytes.extend(std::iter::repeat_n(0xAB_u8, padding));
    }
    ImageFrame::new(width, height, stride, bytes).unwrap()
}

fn pixels(frame: &ImageFrame) -> Vec<Rgba8> {
    (0..frame.height())
        .flat_map(|y| (0..frame.width()).map(move |x| frame.pixel(x, y).unwrap()))
        .collect()
}

fn apply(frame: &ImageFrame, effect: ReferenceEffect) -> ImageFrame {
    apply_reference_effect(frame, effect).unwrap()
}

fn plan_with_effects(width: u32, height: u32, effects: Vec<Effect>) -> CompositionPlan {
    let mut scene = Scene::new(width, height, Rgba8::new(0, 0, 0, 255)).unwrap();
    let mut layer = SourceLayer::new(
        SourceId::new(1),
        0,
        Transform::new(0, 0, width, height, Rotation::Deg0),
    );
    for effect in effects {
        layer = layer.with_effect(effect);
    }
    scene.push_layer(layer);
    compile_scene(&scene, OutputTarget::Program).unwrap().0
}

fn execute_layer(plan: &CompositionPlan, source: &ImageFrame) -> ImageFrame {
    execute_cpu(plan, &[CpuSourceFrame::new(SourceId::new(1), source)]).unwrap()
}

#[test]
fn levels_maps_endpoints_midpoints_and_clamps_each_channel() {
    let source = frame(
        4,
        1,
        &[
            Rgba8::new(0, 0, 0, 17),
            Rgba8::new(25, 50, 75, 17),
            Rgba8::new(100, 101, 10, 17),
            Rgba8::new(8, 9, 200, 17),
        ],
    );
    let output = apply(
        &source,
        ReferenceEffect::Levels {
            black: 0,
            white: 100,
        },
    );
    assert_eq!(
        pixels(&output),
        [
            Rgba8::new(0, 0, 0, 17),
            Rgba8::new(64, 128, 191, 17),
            Rgba8::new(255, 255, 26, 17),
            Rgba8::new(20, 23, 255, 17),
        ]
    );

    let shifted = frame(
        4,
        1,
        &[
            Rgba8::new(8, 8, 8, 33),
            Rgba8::new(9, 9, 9, 33),
            Rgba8::new(10, 10, 10, 33),
            Rgba8::new(200, 200, 200, 33),
        ],
    );
    let output = apply(
        &shifted,
        ReferenceEffect::Levels {
            black: 10,
            white: 200,
        },
    );
    assert_eq!(
        pixels(&output),
        [
            Rgba8::new(0, 0, 0, 33),
            Rgba8::new(0, 0, 0, 33),
            Rgba8::new(0, 0, 0, 33),
            Rgba8::new(255, 255, 255, 33),
        ]
    );
}

#[test]
fn brightness_contrast_rounds_halves_away_from_zero_and_clamps() {
    let rounding = frame(
        3,
        1,
        &[
            Rgba8::new(78, 78, 78, 44),
            Rgba8::new(128, 128, 128, 44),
            Rgba8::new(178, 178, 178, 44),
        ],
    );
    let output = apply(
        &rounding,
        ReferenceEffect::BrightnessContrast {
            brightness: 0,
            contrast: 1,
        },
    );
    assert_eq!(
        pixels(&output),
        [
            Rgba8::new(77, 77, 77, 44),
            Rgba8::new(128, 128, 128, 44),
            Rgba8::new(179, 179, 179, 44),
        ]
    );

    let source = frame(1, 1, &[Rgba8::new(100, 200, 0, 44)]);
    let brighter = apply(
        &source,
        ReferenceEffect::BrightnessContrast {
            brightness: 20,
            contrast: 0,
        },
    );
    assert_eq!(pixels(&brighter), [Rgba8::new(120, 220, 20, 44)]);
    let darker = apply(
        &source,
        ReferenceEffect::BrightnessContrast {
            brightness: -20,
            contrast: 0,
        },
    );
    assert_eq!(pixels(&darker), [Rgba8::new(80, 180, 0, 44)]);
    let more_contrast = apply(
        &source,
        ReferenceEffect::BrightnessContrast {
            brightness: 0,
            contrast: 50,
        },
    );
    assert_eq!(pixels(&more_contrast), [Rgba8::new(86, 236, 0, 44)]);
    let flat = apply(
        &source,
        ReferenceEffect::BrightnessContrast {
            brightness: 0,
            contrast: -100,
        },
    );
    assert_eq!(pixels(&flat), [Rgba8::new(128, 128, 128, 44)]);

    let high = frame(1, 1, &[Rgba8::new(255, 0, 128, 44)]);
    let clamped = apply(
        &high,
        ReferenceEffect::BrightnessContrast {
            brightness: 100,
            contrast: 100,
        },
    );
    assert_eq!(pixels(&clamped), [Rgba8::new(255, 0, 228, 44)]);

    let low = frame(1, 1, &[Rgba8::new(0, 255, 128, 44)]);
    let clamped = apply(
        &low,
        ReferenceEffect::BrightnessContrast {
            brightness: -100,
            contrast: 100,
        },
    );
    assert_eq!(pixels(&clamped), [Rgba8::new(0, 255, 28, 44)]);
}

#[test]
fn saturation_identity_grayscale_and_boost_are_exact() {
    let source = frame(
        3,
        1,
        &[
            Rgba8::new(200, 100, 50, 77),
            Rgba8::new(103, 97, 100, 144),
            Rgba8::new(1, 2, 3, 200),
        ],
    );
    let identity = apply(&source, ReferenceEffect::Saturation { amount: 0 });
    assert_eq!(identity, source);

    let gray = apply(&source, ReferenceEffect::Saturation { amount: -100 });
    assert_eq!(
        pixels(&gray),
        [
            Rgba8::new(124, 124, 124, 77),
            Rgba8::new(99, 99, 99, 144),
            Rgba8::new(2, 2, 2, 200),
        ]
    );

    let quarter = apply(&source, ReferenceEffect::Saturation { amount: -50 });
    assert_eq!(
        pixels(&quarter),
        [
            Rgba8::new(162, 112, 87, 77),
            Rgba8::new(101, 98, 100, 144),
            Rgba8::new(1, 2, 3, 200),
        ]
    );

    let boosted = apply(&source, ReferenceEffect::Saturation { amount: 100 });
    assert_eq!(
        pixels(&boosted),
        [
            Rgba8::new(255, 76, 0, 77),
            Rgba8::new(107, 95, 101, 144),
            Rgba8::new(0, 2, 4, 200),
        ]
    );
}

#[test]
fn sharpen_identity_and_exact_edge_clamped_patterns() {
    let source = frame(
        3,
        2,
        &[
            Rgba8::new(10, 20, 30, 40),
            Rgba8::new(50, 60, 70, 80),
            Rgba8::new(90, 100, 110, 120),
            Rgba8::new(130, 140, 150, 160),
            Rgba8::new(170, 180, 190, 200),
            Rgba8::new(210, 220, 230, 240),
        ],
    );
    assert_eq!(
        apply(&source, ReferenceEffect::Sharpen { amount: 0 }),
        source
    );

    let gradient = frame(
        3,
        3,
        &[
            Rgba8::new(10, 0, 0, 200),
            Rgba8::new(20, 0, 0, 200),
            Rgba8::new(30, 0, 0, 200),
            Rgba8::new(40, 0, 0, 200),
            Rgba8::new(50, 0, 0, 200),
            Rgba8::new(60, 0, 0, 200),
            Rgba8::new(70, 0, 0, 200),
            Rgba8::new(80, 0, 0, 200),
            Rgba8::new(90, 0, 0, 200),
        ],
    );
    let sharpened = apply(&gradient, ReferenceEffect::Sharpen { amount: 1000 });
    assert_eq!(
        pixels(&sharpened),
        [
            Rgba8::new(5, 0, 0, 200),
            Rgba8::new(17, 0, 0, 200),
            Rgba8::new(28, 0, 0, 200),
            Rgba8::new(39, 0, 0, 200),
            Rgba8::new(50, 0, 0, 200),
            Rgba8::new(62, 0, 0, 200),
            Rgba8::new(73, 0, 0, 200),
            Rgba8::new(84, 0, 0, 200),
            Rgba8::new(95, 0, 0, 200),
        ]
    );

    let valley = frame(
        3,
        3,
        &[
            Rgba8::new(51, 0, 0, 200),
            Rgba8::new(51, 0, 0, 200),
            Rgba8::new(51, 0, 0, 200),
            Rgba8::new(51, 0, 0, 200),
            Rgba8::new(49, 0, 0, 200),
            Rgba8::new(51, 0, 0, 200),
            Rgba8::new(51, 0, 0, 200),
            Rgba8::new(51, 0, 0, 200),
            Rgba8::new(51, 0, 0, 200),
        ],
    );
    let sharpened = apply(&valley, ReferenceEffect::Sharpen { amount: 500 });
    assert_eq!(
        pixels(&sharpened),
        [
            Rgba8::new(51, 0, 0, 200),
            Rgba8::new(52, 0, 0, 200),
            Rgba8::new(51, 0, 0, 200),
            Rgba8::new(52, 0, 0, 200),
            Rgba8::new(48, 0, 0, 200),
            Rgba8::new(52, 0, 0, 200),
            Rgba8::new(51, 0, 0, 200),
            Rgba8::new(52, 0, 0, 200),
            Rgba8::new(51, 0, 0, 200),
        ]
    );
}

#[test]
fn flip_maps_all_flag_combinations_exactly() {
    let source = frame(
        3,
        2,
        &[
            Rgba8::new(1, 11, 21, 31),
            Rgba8::new(2, 12, 22, 32),
            Rgba8::new(3, 13, 23, 33),
            Rgba8::new(4, 14, 24, 34),
            Rgba8::new(5, 15, 25, 35),
            Rgba8::new(6, 16, 26, 36),
        ],
    );
    let identity = apply(
        &source,
        ReferenceEffect::Flip {
            horizontal: false,
            vertical: false,
        },
    );
    assert_eq!(identity, source);

    let horizontal = apply(
        &source,
        ReferenceEffect::Flip {
            horizontal: true,
            vertical: false,
        },
    );
    assert_eq!(
        pixels(&horizontal),
        [
            Rgba8::new(3, 13, 23, 33),
            Rgba8::new(2, 12, 22, 32),
            Rgba8::new(1, 11, 21, 31),
            Rgba8::new(6, 16, 26, 36),
            Rgba8::new(5, 15, 25, 35),
            Rgba8::new(4, 14, 24, 34),
        ]
    );

    let vertical = apply(
        &source,
        ReferenceEffect::Flip {
            horizontal: false,
            vertical: true,
        },
    );
    assert_eq!(
        pixels(&vertical),
        [
            Rgba8::new(4, 14, 24, 34),
            Rgba8::new(5, 15, 25, 35),
            Rgba8::new(6, 16, 26, 36),
            Rgba8::new(1, 11, 21, 31),
            Rgba8::new(2, 12, 22, 32),
            Rgba8::new(3, 13, 23, 33),
        ]
    );

    let both = apply(
        &source,
        ReferenceEffect::Flip {
            horizontal: true,
            vertical: true,
        },
    );
    assert_eq!(
        pixels(&both),
        [
            Rgba8::new(6, 16, 26, 36),
            Rgba8::new(5, 15, 25, 35),
            Rgba8::new(4, 14, 24, 34),
            Rgba8::new(3, 13, 23, 33),
            Rgba8::new(2, 12, 22, 32),
            Rgba8::new(1, 11, 21, 31),
        ]
    );
}

#[test]
fn strided_inputs_are_read_per_pixel_and_outputs_are_tightly_packed() {
    let source_pixels = [
        Rgba8::new(10, 20, 30, 40),
        Rgba8::new(50, 60, 70, 80),
        Rgba8::new(90, 100, 110, 120),
        Rgba8::new(130, 140, 150, 160),
        Rgba8::new(170, 180, 190, 200),
        Rgba8::new(210, 220, 230, 240),
    ];
    let padded = padded_frame(3, 2, &source_pixels, 4);
    assert_eq!(padded.stride(), 16);

    let flipped = apply(
        &padded,
        ReferenceEffect::Flip {
            horizontal: true,
            vertical: true,
        },
    );
    assert_eq!(flipped.stride(), 12);
    assert_eq!(
        pixels(&flipped),
        [
            source_pixels[5],
            source_pixels[4],
            source_pixels[3],
            source_pixels[2],
            source_pixels[1],
            source_pixels[0],
        ]
    );

    let tight = frame(3, 2, &source_pixels);
    let effect = ReferenceEffect::BrightnessContrast {
        brightness: 10,
        contrast: 0,
    };
    let adjusted = apply(&padded, effect);
    assert_eq!(adjusted, apply(&tight, effect));
    assert_eq!(adjusted.stride(), 12);
}

#[test]
fn from_descriptor_accepts_every_documented_schema() {
    let cases = [
        (
            ReferenceEffect::Levels {
                black: 0,
                white: 255,
            },
            vec![0, 255],
        ),
        (
            ReferenceEffect::BrightnessContrast {
                brightness: -100,
                contrast: 100,
            },
            vec![-100, 100],
        ),
        (ReferenceEffect::Saturation { amount: 100 }, vec![100]),
        (ReferenceEffect::Sharpen { amount: 1000 }, vec![1000]),
        (
            ReferenceEffect::Flip {
                horizontal: true,
                vertical: false,
            },
            vec![1, 0],
        ),
    ];
    for (effect, parameters) in cases {
        let descriptor = Effect::new(effect.name(), parameters);
        assert_eq!(ReferenceEffect::from_descriptor(&descriptor), Ok(effect));
    }
}

#[test]
fn from_descriptor_rejects_empty_parameter_vectors_for_every_known_name() {
    let cases = [
        ("levels", 2),
        ("brightness_contrast", 2),
        ("saturation", 1),
        ("sharpen", 1),
        ("flip", 2),
    ];
    for (name, expected) in cases {
        let descriptor = Effect::new(name, Vec::new());
        assert_eq!(
            ReferenceEffect::from_descriptor(&descriptor),
            Err(ReferenceEffectError::WrongParameterCount {
                name: name.to_owned(),
                expected,
                actual: 0,
            })
        );
    }
}

#[test]
fn from_descriptor_rejects_unknown_names_and_wrong_counts() {
    assert_eq!(
        ReferenceEffect::from_descriptor(&Effect::new("gpu-only", vec![1])),
        Err(ReferenceEffectError::UnknownEffect {
            name: "gpu-only".to_owned(),
        })
    );
    assert_eq!(
        ReferenceEffect::from_descriptor(&Effect::new("levels", vec![1])),
        Err(ReferenceEffectError::WrongParameterCount {
            name: "levels".to_owned(),
            expected: 2,
            actual: 1,
        })
    );
    assert_eq!(
        ReferenceEffect::from_descriptor(&Effect::new("brightness_contrast", vec![0, 0, 0])),
        Err(ReferenceEffectError::WrongParameterCount {
            name: "brightness_contrast".to_owned(),
            expected: 2,
            actual: 3,
        })
    );
}

#[test]
fn from_descriptor_rejects_out_of_range_parameters_and_invalid_levels() {
    assert_eq!(
        ReferenceEffect::from_descriptor(&Effect::new("levels", vec![-1, 255])),
        Err(ReferenceEffectError::ParameterOutOfRange {
            name: "levels".to_owned(),
            parameter: 0,
            value: -1,
            minimum: 0,
            maximum: 255,
        })
    );
    assert_eq!(
        ReferenceEffect::from_descriptor(&Effect::new("levels", vec![0, 256])),
        Err(ReferenceEffectError::ParameterOutOfRange {
            name: "levels".to_owned(),
            parameter: 1,
            value: 256,
            minimum: 0,
            maximum: 255,
        })
    );
    assert_eq!(
        ReferenceEffect::from_descriptor(&Effect::new("brightness_contrast", vec![-101, 0])),
        Err(ReferenceEffectError::ParameterOutOfRange {
            name: "brightness_contrast".to_owned(),
            parameter: 0,
            value: -101,
            minimum: -100,
            maximum: 100,
        })
    );
    assert_eq!(
        ReferenceEffect::from_descriptor(&Effect::new("saturation", vec![101])),
        Err(ReferenceEffectError::ParameterOutOfRange {
            name: "saturation".to_owned(),
            parameter: 0,
            value: 101,
            minimum: -100,
            maximum: 100,
        })
    );
    assert_eq!(
        ReferenceEffect::from_descriptor(&Effect::new("sharpen", vec![1001])),
        Err(ReferenceEffectError::ParameterOutOfRange {
            name: "sharpen".to_owned(),
            parameter: 0,
            value: 1001,
            minimum: 0,
            maximum: 1000,
        })
    );
    assert_eq!(
        ReferenceEffect::from_descriptor(&Effect::new("flip", vec![2, 0])),
        Err(ReferenceEffectError::ParameterOutOfRange {
            name: "flip".to_owned(),
            parameter: 0,
            value: 2,
            minimum: 0,
            maximum: 1,
        })
    );
    assert_eq!(
        ReferenceEffect::from_descriptor(&Effect::new("flip", vec![0, -1])),
        Err(ReferenceEffectError::ParameterOutOfRange {
            name: "flip".to_owned(),
            parameter: 1,
            value: -1,
            minimum: 0,
            maximum: 1,
        })
    );
    assert_eq!(
        ReferenceEffect::from_descriptor(&Effect::new("levels", vec![100, 100])),
        Err(ReferenceEffectError::InvalidLevels {
            black: 100,
            white: 100,
        })
    );
    assert_eq!(
        ReferenceEffect::from_descriptor(&Effect::new("levels", vec![200, 10])),
        Err(ReferenceEffectError::InvalidLevels {
            black: 200,
            white: 10,
        })
    );
}

#[test]
fn execute_cpu_distinguishes_unknown_from_invalid_known_effects() {
    let mut scene = Scene::new(2, 1, Rgba8::new(0, 0, 0, 255)).unwrap();
    scene.push_layer(
        SourceLayer::new(
            SourceId::new(1),
            0,
            Transform::new(0, 0, 1, 1, Rotation::Deg0),
        )
        .with_effect(Effect::new("levels", vec![0, 255])),
    );
    scene.push_layer(
        SourceLayer::new(
            SourceId::new(2),
            1,
            Transform::new(1, 0, 1, 1, Rotation::Deg0),
        )
        .with_effect(Effect::new("gpu-only", vec![1])),
    );
    let (plan, _) = compile_scene(&scene, OutputTarget::Program).unwrap();
    let first = frame(1, 1, &[Rgba8::new(10, 20, 30, 255)]);
    let second = frame(1, 1, &[Rgba8::new(40, 50, 60, 255)]);
    let error = execute_cpu(
        &plan,
        &[
            CpuSourceFrame::new(SourceId::new(1), &first),
            CpuSourceFrame::new(SourceId::new(2), &second),
        ],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CpuExecutionError::UnsupportedEffect { layer: 1, effect: 0, name } if name == "gpu-only"
    ));

    let source = frame(1, 1, &[Rgba8::new(0, 0, 0, 255)]);
    let invalid = plan_with_effects(1, 1, vec![Effect::new("levels", vec![200, 200])]);
    let error =
        execute_cpu(&invalid, &[CpuSourceFrame::new(SourceId::new(1), &source)]).unwrap_err();
    let CpuExecutionError::InvalidEffect {
        layer,
        effect,
        name,
        reason,
    } = error
    else {
        panic!("expected InvalidEffect");
    };
    assert_eq!((layer, effect), (0, 0));
    assert_eq!(name, "levels");
    assert_eq!(
        reason,
        ReferenceEffectError::InvalidLevels {
            black: 200,
            white: 200,
        }
        .to_string()
    );
}

#[test]
fn execute_cpu_applies_effects_in_declared_order() {
    let source = frame(1, 1, &[Rgba8::new(150, 50, 0, 255)]);
    let ordered = plan_with_effects(
        1,
        1,
        vec![
            Effect::new("levels", vec![0, 100]),
            Effect::new("brightness_contrast", vec![-50, 0]),
        ],
    );
    let output = execute_layer(&ordered, &source);
    assert_eq!(output.pixel(0, 0), Some(Rgba8::new(205, 78, 0, 255)));

    let reversed = plan_with_effects(
        1,
        1,
        vec![
            Effect::new("brightness_contrast", vec![-50, 0]),
            Effect::new("levels", vec![0, 100]),
        ],
    );
    let output = execute_layer(&reversed, &source);
    assert_eq!(output.pixel(0, 0), Some(Rgba8::new(255, 0, 0, 255)));
}

#[test]
fn parameterless_passthrough_stays_a_no_op() {
    let source = frame(2, 1, &[Rgba8::new(1, 2, 3, 255), Rgba8::new(4, 5, 6, 255)]);
    let plan = plan_with_effects(2, 1, vec![Effect::passthrough()]);
    let output = execute_layer(&plan, &source);
    assert_eq!(
        pixels(&output),
        [Rgba8::new(1, 2, 3, 255), Rgba8::new(4, 5, 6, 255)]
    );
}

#[test]
fn repeated_execution_is_byte_identical() {
    let source = frame(
        2,
        2,
        &[
            Rgba8::new(10, 90, 200, 255),
            Rgba8::new(120, 30, 60, 255),
            Rgba8::new(240, 210, 180, 255),
            Rgba8::new(7, 8, 9, 255),
        ],
    );
    let plan = plan_with_effects(
        2,
        2,
        vec![
            Effect::new("levels", vec![10, 200]),
            Effect::new("saturation", vec![40]),
            Effect::new("sharpen", vec![750]),
            Effect::new("flip", vec![1, 1]),
        ],
    );
    let first = execute_layer(&plan, &source);
    let second = execute_layer(&plan, &source);
    assert_eq!(first, second);
    assert_eq!(first.pixels(), second.pixels());
}

fn execute_with_alpha_mode(
    alpha_mode: fm_compositor::AlphaMode,
    effects: &[Effect],
    source: &ImageFrame,
) -> Result<ImageFrame, CpuExecutionError> {
    let mut scene = Scene::new(source.width(), source.height(), Rgba8::new(0, 0, 0, 255)).unwrap();
    let mut layer = SourceLayer::new(
        SourceId::new(1),
        0,
        Transform::new(0, 0, source.width(), source.height(), Rotation::Deg0),
    )
    .with_alpha_mode(alpha_mode);
    for effect in effects {
        layer = layer.with_effect(effect.clone());
    }
    scene.push_layer(layer);
    let plan = compile_scene(&scene, OutputTarget::Program).unwrap().0;
    execute_cpu(&plan, &[CpuSourceFrame::new(SourceId::new(1), source)])
}

#[test]
fn premultiplied_layers_run_color_effects_on_straight_color() {
    use fm_compositor::AlphaMode;

    // The same visible content, once straight and once premultiplied. A
    // brightening effect must not lift a transparent pixel's color above its
    // zero alpha, and must treat a half-transparent pixel by its true color.
    let straight = frame(
        3,
        1,
        &[
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(128, 64, 0, 128),
            Rgba8::new(10, 20, 30, 255),
        ],
    );
    let premultiplied = frame(
        3,
        1,
        &[
            Rgba8::new(0, 0, 0, 0),
            Rgba8::new(64, 32, 0, 128),
            Rgba8::new(10, 20, 30, 255),
        ],
    );
    let effects = [
        Effect::new("brightness_contrast", vec![100, 0]),
        Effect::new("levels", vec![0, 200]),
    ];
    let expected = execute_with_alpha_mode(AlphaMode::Straight, &effects, &straight).unwrap();
    let actual =
        execute_with_alpha_mode(AlphaMode::Premultiplied, &effects, &premultiplied).unwrap();
    assert_eq!(pixels(&actual), pixels(&expected));
    assert_eq!(actual.pixel(0, 0), Some(Rgba8::new(0, 0, 0, 255)));
}

#[test]
fn premultiplied_layers_round_trip_every_pixel_through_identity_effects() {
    use fm_compositor::AlphaMode;

    let mut source = Vec::new();
    for alpha in 0..=255_u8 {
        for x in 0..=255_u8 {
            let color = x.min(alpha);
            source.push(Rgba8::new(color, alpha - color, color / 2, alpha));
        }
    }
    let source = frame(256, 256, &source);
    let identity = [Effect::new("saturation", vec![0])];
    let with_effect =
        execute_with_alpha_mode(AlphaMode::Premultiplied, &identity, &source).unwrap();
    let without = execute_with_alpha_mode(AlphaMode::Premultiplied, &[], &source).unwrap();
    assert_eq!(with_effect, without);
}
