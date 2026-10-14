use core::num::NonZeroU128;

use fm_compositor::{
    ConfidenceMode, CpuSourceFrame, GridLayout, MAX_MULTIVIEW_CELLS, MAX_MULTIVIEW_COLUMNS,
    MAX_MULTIVIEW_NAME_BYTES, MAX_MULTIVIEW_ROWS, MultiviewCell, MultiviewDefinition,
    MultiviewError, MultiviewOverlays, MultiviewPlan, MultiviewSource, OutputTarget, Rgba8, Scene,
    SceneError, SourceId, SourceLayer, compile_multiview_scene, compile_scene, execute_cpu,
    plan_multiview,
};
use fm_types::InputId;
use fm_video::ImageFrame;

const BACKGROUND: Rgba8 = Rgba8::new(0, 0, 0, 255);
const WHITE: Rgba8 = Rgba8::new(255, 255, 255, 255);

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn source(value: u64) -> SourceId {
    SourceId::new(value)
}

fn frame(width: u32, height: u32, pixels: &[Rgba8]) -> ImageFrame {
    ImageFrame::new(
        width,
        height,
        usize::try_from(width).unwrap() * 4,
        pixels.iter().flat_map(|pixel| pixel.to_bytes()).collect(),
    )
    .unwrap()
}

fn pixels(frame: &ImageFrame) -> Vec<Rgba8> {
    (0..frame.height())
        .flat_map(|y| (0..frame.width()).map(move |x| frame.pixel(x, y).unwrap()))
        .collect()
}

fn definition(
    name: &str,
    layout: GridLayout,
    sources: Vec<MultiviewSource>,
) -> MultiviewDefinition {
    MultiviewDefinition::new(
        name,
        layout,
        sources,
        ConfidenceMode::None,
        MultiviewOverlays::none(),
    )
    .unwrap()
}

fn quad_definition() -> MultiviewDefinition {
    definition(
        "quad",
        GridLayout::two_by_two(),
        vec![
            MultiviewSource::Program,
            MultiviewSource::Preview,
            MultiviewSource::Input(input(7)),
            MultiviewSource::Input(input(9)),
        ],
    )
}

fn resolve_quad(cell: MultiviewSource) -> Option<SourceId> {
    match cell {
        MultiviewSource::Program => Some(source(1)),
        MultiviewSource::Preview => Some(source(2)),
        MultiviewSource::Input(id) if id == input(7) => Some(source(3)),
        MultiviewSource::Input(id) if id == input(9) => Some(source(4)),
        MultiviewSource::Input(_) => None,
    }
}

fn render(scene: &Scene, sources: &[CpuSourceFrame<'_>]) -> ImageFrame {
    let (plan, _) = compile_scene(scene, OutputTarget::Program).unwrap();
    execute_cpu(&plan, sources).unwrap()
}

fn assert_exact_tiling(plan: &MultiviewPlan, width: u32, height: u32) {
    let area = plan
        .cells()
        .iter()
        .map(|cell| cell.width * cell.height)
        .sum::<u32>();
    assert_eq!(area, width * height);
    for y in 0..height {
        for x in 0..width {
            let containing = plan
                .cells()
                .iter()
                .filter(|cell| cell.contains(x, y))
                .count();
            assert_eq!(containing, 1, "pixel ({x}, {y}) must belong to one cell");
            assert!(plan.hit_test(x, y).is_some(), "pixel ({x}, {y}) is empty");
        }
    }
}

#[test]
fn grid_layout_validates_dimensions_and_reports_capacity() {
    assert_eq!(GridLayout::new(0, 1), Err(MultiviewError::ZeroGridColumns));
    assert_eq!(GridLayout::new(1, 0), Err(MultiviewError::ZeroGridRows));
    assert_eq!(GridLayout::new(0, 0), Err(MultiviewError::ZeroGridColumns));
    assert_eq!(
        GridLayout::new(MAX_MULTIVIEW_COLUMNS + 1, 1),
        Err(MultiviewError::GridColumnsExceeded {
            columns: 5,
            maximum: 4,
        })
    );
    assert_eq!(
        GridLayout::new(1, MAX_MULTIVIEW_ROWS + 1),
        Err(MultiviewError::GridRowsExceeded {
            rows: 5,
            maximum: 4,
        })
    );
    assert_eq!(
        GridLayout::new(5, 5),
        Err(MultiviewError::GridColumnsExceeded {
            columns: 5,
            maximum: 4,
        })
    );

    for (layout, columns, rows) in [
        (GridLayout::one_by_one(), 1_u8, 1_u8),
        (GridLayout::two_by_two(), 2, 2),
        (GridLayout::three_by_three(), 3, 3),
        (GridLayout::four_by_four(), 4, 4),
    ] {
        assert_eq!(layout.columns(), columns);
        assert_eq!(layout.rows(), rows);
        assert_eq!(layout.capacity(), usize::from(columns) * usize::from(rows));
        assert_eq!(GridLayout::new(columns, rows), Ok(layout));
    }
    assert_eq!(GridLayout::four_by_four().capacity(), MAX_MULTIVIEW_CELLS);
}

#[test]
fn planning_geometry_is_exact_for_divisible_and_remainder_canvases() {
    let single = definition(
        "single",
        GridLayout::one_by_one(),
        vec![MultiviewSource::Program],
    );
    let plan = plan_multiview(3, 2, &single).unwrap();
    assert_eq!(plan.len(), 1);
    assert!(!plan.is_empty());
    assert_eq!(plan.cell(1), None);
    let cell = plan.cell(0).unwrap();
    assert_eq!((cell.row, cell.column), (0, 0));
    assert_eq!((cell.x, cell.y, cell.width, cell.height), (0, 0, 3, 2));
    assert_eq!(plan.hit_test(2, 1), Some(MultiviewSource::Program));
    assert_exact_tiling(&plan, 3, 2);

    let quad = quad_definition();
    let plan = plan_multiview(4, 4, &quad).unwrap();
    let expected = [
        MultiviewCell {
            source: MultiviewSource::Program,
            row: 0,
            column: 0,
            x: 0,
            y: 0,
            width: 2,
            height: 2,
        },
        MultiviewCell {
            source: MultiviewSource::Preview,
            row: 0,
            column: 1,
            x: 2,
            y: 0,
            width: 2,
            height: 2,
        },
        MultiviewCell {
            source: MultiviewSource::Input(input(7)),
            row: 1,
            column: 0,
            x: 0,
            y: 2,
            width: 2,
            height: 2,
        },
        MultiviewCell {
            source: MultiviewSource::Input(input(9)),
            row: 1,
            column: 1,
            x: 2,
            y: 2,
            width: 2,
            height: 2,
        },
    ];
    assert_eq!(plan.cells(), &expected[..]);
    assert_exact_tiling(&plan, 4, 4);

    // 5x3 at 2x2: the base sizes are 2x1 and the leftover column/row pixels go
    // to the leftmost column and topmost row.
    let plan = plan_multiview(5, 3, &quad).unwrap();
    let expected = [
        MultiviewCell {
            source: MultiviewSource::Program,
            row: 0,
            column: 0,
            x: 0,
            y: 0,
            width: 3,
            height: 2,
        },
        MultiviewCell {
            source: MultiviewSource::Preview,
            row: 0,
            column: 1,
            x: 3,
            y: 0,
            width: 2,
            height: 2,
        },
        MultiviewCell {
            source: MultiviewSource::Input(input(7)),
            row: 1,
            column: 0,
            x: 0,
            y: 2,
            width: 3,
            height: 1,
        },
        MultiviewCell {
            source: MultiviewSource::Input(input(9)),
            row: 1,
            column: 1,
            x: 3,
            y: 2,
            width: 2,
            height: 1,
        },
    ];
    assert_eq!(plan.cells(), &expected[..]);
    assert_exact_tiling(&plan, 5, 3);
}

#[test]
fn planning_spreads_remainders_across_a_three_by_three_grid() {
    let nine = definition(
        "nine",
        GridLayout::three_by_three(),
        (1..=9)
            .map(|value| MultiviewSource::Input(input(value)))
            .collect(),
    );
    let plan = plan_multiview(5, 5, &nine).unwrap();
    let geometry = plan
        .cells()
        .iter()
        .map(|cell| (cell.x, cell.y, cell.width, cell.height))
        .collect::<Vec<_>>();
    assert_eq!(
        geometry,
        vec![
            (0, 0, 2, 2),
            (2, 0, 2, 2),
            (4, 0, 1, 2),
            (0, 2, 2, 2),
            (2, 2, 2, 2),
            (4, 2, 1, 2),
            (0, 4, 2, 1),
            (2, 4, 2, 1),
            (4, 4, 1, 1),
        ]
    );
    assert_exact_tiling(&plan, 5, 5);
}

#[test]
fn hit_test_boundaries_resolve_to_exactly_one_cell() {
    let plan = plan_multiview(5, 3, &quad_definition()).unwrap();
    for cell in plan.cells() {
        let corners = [
            (cell.x, cell.y),
            (cell.x + cell.width - 1, cell.y),
            (cell.x, cell.y + cell.height - 1),
            (cell.x + cell.width - 1, cell.y + cell.height - 1),
        ];
        for (x, y) in corners {
            assert_eq!(plan.hit_test(x, y), Some(cell.source), "corner ({x}, {y})");
            assert_eq!(
                plan.cells()
                    .iter()
                    .filter(|candidate| candidate.contains(x, y))
                    .count(),
                1,
                "corner ({x}, {y})"
            );
        }
    }

    // Internal borders: the first column is 3 wide and the first row 2 tall.
    assert_eq!(plan.hit_test(2, 0), Some(MultiviewSource::Program));
    assert_eq!(plan.hit_test(3, 0), Some(MultiviewSource::Preview));
    assert_eq!(plan.hit_test(0, 1), Some(MultiviewSource::Program));
    assert_eq!(plan.hit_test(0, 2), Some(MultiviewSource::Input(input(7))));
    assert_eq!(plan.hit_test(3, 2), Some(MultiviewSource::Input(input(9))));

    assert_eq!(plan.hit_test(5, 0), None);
    assert_eq!(plan.hit_test(0, 3), None);
    assert_eq!(plan.hit_test(u32::MAX, u32::MAX), None);
}

#[test]
fn definitions_reject_empty_oversized_and_duplicate_sources() {
    let layout = GridLayout::two_by_two();
    let overlays = MultiviewOverlays::none();
    let build = |sources: Vec<MultiviewSource>| {
        MultiviewDefinition::new("view", layout, sources, ConfidenceMode::None, overlays)
    };

    assert_eq!(build(Vec::new()), Err(MultiviewError::NoSources));
    assert_eq!(
        build(vec![
            MultiviewSource::Program,
            MultiviewSource::Preview,
            MultiviewSource::Input(input(1)),
            MultiviewSource::Input(input(2)),
            MultiviewSource::Input(input(3)),
        ]),
        Err(MultiviewError::SourceCapacityExceeded {
            actual: 5,
            capacity: 4,
        })
    );
    assert_eq!(
        build(vec![MultiviewSource::Program, MultiviewSource::Program]),
        Err(MultiviewError::DuplicateSource {
            source: MultiviewSource::Program,
        })
    );
    assert_eq!(
        build(vec![
            MultiviewSource::Input(input(1)),
            MultiviewSource::Input(input(1)),
        ]),
        Err(MultiviewError::DuplicateSource {
            source: MultiviewSource::Input(input(1)),
        })
    );

    let sixteen = (1..=16)
        .map(|value| MultiviewSource::Input(input(value)))
        .collect::<Vec<_>>();
    let full = MultiviewDefinition::new(
        "full",
        GridLayout::four_by_four(),
        sixteen,
        ConfidenceMode::None,
        overlays,
    )
    .unwrap();
    assert_eq!(full.sources.len(), MAX_MULTIVIEW_CELLS);

    let seventeen = (1..=17)
        .map(|value| MultiviewSource::Input(input(value)))
        .collect::<Vec<_>>();
    let oversized = MultiviewDefinition {
        name: "oversized".to_owned(),
        layout: GridLayout::four_by_four(),
        sources: seventeen,
        confidence: ConfidenceMode::None,
        overlays: MultiviewOverlays::none(),
    };
    assert_eq!(
        oversized.validate(),
        Err(MultiviewError::SourceLimitExceeded {
            actual: 17,
            maximum: MAX_MULTIVIEW_CELLS,
        })
    );
    assert_eq!(
        plan_multiview(4, 4, &oversized),
        Err(MultiviewError::SourceLimitExceeded {
            actual: 17,
            maximum: MAX_MULTIVIEW_CELLS,
        })
    );
}

#[test]
fn definitions_enforce_bounded_control_free_names() {
    let layout = GridLayout::one_by_one();
    let overlays = MultiviewOverlays::none();
    let build = |name: &str| {
        MultiviewDefinition::new(
            name,
            layout,
            vec![MultiviewSource::Program],
            ConfidenceMode::None,
            overlays,
        )
    };

    assert_eq!(build(""), Err(MultiviewError::EmptyName));
    assert_eq!(
        build(&"x".repeat(MAX_MULTIVIEW_NAME_BYTES + 1)),
        Err(MultiviewError::NameTooLong {
            actual: MAX_MULTIVIEW_NAME_BYTES + 1,
            maximum: MAX_MULTIVIEW_NAME_BYTES,
        })
    );
    assert_eq!(
        build("multi\nview"),
        Err(MultiviewError::ControlCharacterInName { index: 5 })
    );
    assert_eq!(
        build("multi\u{7f}view"),
        Err(MultiviewError::ControlCharacterInName { index: 5 })
    );
    assert!(build(&"x".repeat(MAX_MULTIVIEW_NAME_BYTES)).is_ok());
    assert!(build("multiview-\u{fc}").is_ok());
}

#[test]
fn single_input_confidence_must_be_a_cell_source() {
    let layout = GridLayout::two_by_two();
    let overlays = MultiviewOverlays::NONE;
    assert_eq!(
        MultiviewDefinition::new(
            "confidence",
            layout,
            vec![MultiviewSource::Program, MultiviewSource::Input(input(1))],
            ConfidenceMode::SingleInput(input(2)),
            overlays,
        ),
        Err(MultiviewError::SingleInputNotPresent { input: input(2) })
    );

    let definition = MultiviewDefinition::new(
        "confidence",
        layout,
        vec![MultiviewSource::Program, MultiviewSource::Input(input(1))],
        ConfidenceMode::SingleInput(input(1)),
        overlays,
    )
    .unwrap();
    assert_eq!(definition.confidence, ConfidenceMode::SingleInput(input(1)));
    assert!(
        MultiviewDefinition::new(
            "confidence",
            layout,
            vec![MultiviewSource::Program],
            ConfidenceMode::InputsOnly,
            overlays,
        )
        .is_ok()
    );
}

#[test]
fn planning_revalidates_hand_built_definitions() {
    let duplicate = MultiviewDefinition {
        name: "duplicate".to_owned(),
        layout: GridLayout::two_by_two(),
        sources: vec![MultiviewSource::Program, MultiviewSource::Program],
        confidence: ConfidenceMode::None,
        overlays: MultiviewOverlays::none(),
    };
    assert_eq!(
        duplicate.validate(),
        Err(MultiviewError::DuplicateSource {
            source: MultiviewSource::Program,
        })
    );
    assert_eq!(
        plan_multiview(4, 4, &duplicate),
        Err(MultiviewError::DuplicateSource {
            source: MultiviewSource::Program,
        })
    );

    let oversized = MultiviewDefinition {
        name: "oversized".to_owned(),
        layout: GridLayout::one_by_one(),
        sources: vec![MultiviewSource::Program, MultiviewSource::Preview],
        confidence: ConfidenceMode::None,
        overlays: MultiviewOverlays::none(),
    };
    assert_eq!(
        plan_multiview(4, 4, &oversized),
        Err(MultiviewError::SourceCapacityExceeded {
            actual: 2,
            capacity: 1,
        })
    );

    let unnamed = MultiviewDefinition {
        name: String::new(),
        layout: GridLayout::one_by_one(),
        sources: vec![MultiviewSource::Program],
        confidence: ConfidenceMode::None,
        overlays: MultiviewOverlays::none(),
    };
    assert_eq!(
        plan_multiview(4, 4, &unnamed),
        Err(MultiviewError::EmptyName)
    );
}

#[test]
fn planning_rejects_zero_and_undersized_canvases() {
    let quad = quad_definition();
    assert_eq!(
        plan_multiview(0, 3, &quad),
        Err(MultiviewError::ZeroCanvasWidth)
    );
    assert_eq!(
        plan_multiview(3, 0, &quad),
        Err(MultiviewError::ZeroCanvasHeight)
    );
    assert_eq!(
        plan_multiview(0, 0, &quad),
        Err(MultiviewError::ZeroCanvasWidth)
    );
    assert_eq!(
        plan_multiview(4, 1, &quad),
        Err(MultiviewError::CanvasSmallerThanGrid {
            width: 4,
            height: 1,
            columns: 2,
            rows: 2,
        })
    );
    assert_eq!(
        plan_multiview(1, 3, &quad),
        Err(MultiviewError::CanvasSmallerThanGrid {
            width: 1,
            height: 3,
            columns: 2,
            rows: 2,
        })
    );
    // 3x3 is larger than the 2x2 grid in both dimensions, so cells may still be
    // only two or one pixels on a side.
    let plan = plan_multiview(3, 3, &quad).unwrap();
    assert_eq!(
        plan.cells()
            .iter()
            .map(|cell| (cell.x, cell.y, cell.width, cell.height))
            .collect::<Vec<_>>(),
        vec![(0, 0, 2, 2), (2, 0, 1, 2), (0, 2, 2, 1), (2, 2, 1, 1)]
    );
    assert!(plan_multiview(2, 2, &quad).is_ok());

    let grid = definition(
        "grid",
        GridLayout::four_by_four(),
        (1..=16)
            .map(|value| MultiviewSource::Input(input(value)))
            .collect(),
    );
    assert_eq!(
        plan_multiview(3, 4, &grid),
        Err(MultiviewError::CanvasSmallerThanGrid {
            width: 3,
            height: 4,
            columns: 4,
            rows: 4,
        })
    );
    assert!(plan_multiview(4, 4, &grid).is_ok());
}

#[test]
fn compile_and_execute_render_exact_pixels_and_scaled_blocks() {
    let definition = quad_definition();
    let colors = [
        Rgba8::new(10, 0, 0, 255),
        Rgba8::new(0, 20, 0, 255),
        Rgba8::new(0, 0, 30, 255),
        Rgba8::new(40, 40, 40, 255),
    ];
    let frames = colors.map(|color| frame(1, 1, &[color]));
    let cpu_sources = [
        CpuSourceFrame::new(source(1), &frames[0]),
        CpuSourceFrame::new(source(2), &frames[1]),
        CpuSourceFrame::new(source(3), &frames[2]),
        CpuSourceFrame::new(source(4), &frames[3]),
    ];

    let plan = plan_multiview(2, 2, &definition).unwrap();
    let scene = compile_multiview_scene(&plan, 2, 2, BACKGROUND, 0, resolve_quad).unwrap();
    assert_eq!(scene.width(), 2);
    assert_eq!(scene.height(), 2);
    assert_eq!(scene.layers().len(), 4);
    assert_eq!(
        scene
            .layers()
            .iter()
            .map(SourceLayer::z)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(pixels(&render(&scene, &cpu_sources)), colors.to_vec());

    let plan = plan_multiview(4, 4, &definition).unwrap();
    let scene = compile_multiview_scene(&plan, 4, 4, BACKGROUND, 0, resolve_quad).unwrap();
    let expected = vec![
        colors[0], colors[0], colors[1], colors[1], colors[0], colors[0], colors[1], colors[1],
        colors[2], colors[2], colors[3], colors[3], colors[2], colors[2], colors[3], colors[3],
    ];
    assert_eq!(pixels(&render(&scene, &cpu_sources)), expected);
}

#[test]
fn inset_border_paints_inside_each_tile() {
    let definition = quad_definition();
    let colors = [
        Rgba8::new(10, 0, 0, 255),
        Rgba8::new(0, 20, 0, 255),
        Rgba8::new(0, 0, 30, 255),
        Rgba8::new(40, 40, 40, 255),
    ];
    let frames = colors.map(|color| frame(1, 1, &[color]));
    let cpu_sources = [
        CpuSourceFrame::new(source(1), &frames[0]),
        CpuSourceFrame::new(source(2), &frames[1]),
        CpuSourceFrame::new(source(3), &frames[2]),
        CpuSourceFrame::new(source(4), &frames[3]),
    ];

    // 6x6 canvas with a 2x2 grid produces 3x3 tiles, leaving a visible center.
    let plan = plan_multiview(6, 6, &definition).unwrap();
    let unbordered = compile_multiview_scene(&plan, 6, 6, BACKGROUND, 0, resolve_quad).unwrap();
    let unbordered_output = render(&unbordered, &cpu_sources);
    assert_eq!(unbordered_output.pixel(0, 0), Some(colors[0]));
    assert_eq!(unbordered_output.pixel(1, 1), Some(colors[0]));

    let bordered = compile_multiview_scene(&plan, 6, 6, BACKGROUND, 1, resolve_quad).unwrap();
    for (index, layer) in bordered.layers().iter().enumerate() {
        assert_eq!(layer.inset_border_width(), 1);
        assert_eq!(layer.z(), i32::try_from(index).unwrap());
    }
    let output = render(&bordered, &cpu_sources);
    for (index, (row, column)) in [(0_u32, 0_u32), (0, 1), (1, 0), (1, 1)]
        .into_iter()
        .enumerate()
    {
        let left = column * 3;
        let top = row * 3;
        for y in 0..3 {
            for x in 0..3 {
                let expected = if x == 1 && y == 1 {
                    colors[index]
                } else {
                    WHITE
                };
                assert_eq!(
                    output.pixel(left + x, top + y),
                    Some(expected),
                    "tile ({row}, {column}) pixel ({x}, {y})"
                );
            }
        }
    }
}

#[test]
fn empty_cells_show_background_and_are_not_hit_testable() {
    let definition = definition(
        "partial",
        GridLayout::two_by_two(),
        vec![
            MultiviewSource::Program,
            MultiviewSource::Preview,
            MultiviewSource::Input(input(5)),
        ],
    );
    let plan = plan_multiview(6, 6, &definition).unwrap();
    assert_eq!(plan.len(), 3);
    assert_eq!(plan.hit_test(1, 1), Some(MultiviewSource::Program));
    assert_eq!(plan.hit_test(4, 1), Some(MultiviewSource::Preview));
    assert_eq!(plan.hit_test(1, 4), Some(MultiviewSource::Input(input(5))));
    assert_eq!(plan.hit_test(4, 4), None);

    let source_color = Rgba8::new(1, 2, 3, 255);
    let frames = [frame(1, 1, &[source_color])];
    let scene = compile_multiview_scene(&plan, 6, 6, BACKGROUND, 0, |_| Some(source(1))).unwrap();
    assert_eq!(scene.layers().len(), 3);
    let output = render(&scene, &[CpuSourceFrame::new(source(1), &frames[0])]);
    assert_eq!(output.pixel(1, 1), Some(source_color));
    assert_eq!(output.pixel(4, 4), Some(BACKGROUND));
    assert_eq!(output.pixel(5, 5), Some(BACKGROUND));
}

#[test]
fn unresolved_cell_sources_and_invalid_scenes_are_rejected() {
    let plan = plan_multiview(2, 2, &quad_definition()).unwrap();
    assert_eq!(
        compile_multiview_scene(&plan, 2, 2, BACKGROUND, 0, |cell| match cell {
            MultiviewSource::Program => Some(source(1)),
            _ => None,
        }),
        Err(MultiviewError::UnresolvedSource {
            source: MultiviewSource::Preview,
        })
    );

    let not_premultiplied = Rgba8::new(255, 0, 0, 0);
    assert_eq!(
        compile_multiview_scene(&plan, 2, 2, not_premultiplied, 0, resolve_quad),
        Err(MultiviewError::Scene(
            SceneError::BackgroundNotPremultiplied(not_premultiplied,)
        ))
    );

    assert_eq!(
        compile_multiview_scene(&plan, 0, 2, BACKGROUND, 0, resolve_quad),
        Err(MultiviewError::ZeroCanvasWidth)
    );
    assert_eq!(
        compile_multiview_scene(&plan, 2, 0, BACKGROUND, 0, resolve_quad),
        Err(MultiviewError::ZeroCanvasHeight)
    );
}

#[test]
fn independent_definitions_plan_and_compile_separately() {
    let program = definition(
        "program",
        GridLayout::one_by_one(),
        vec![MultiviewSource::Program],
    );
    let quad = MultiviewDefinition::new(
        "quad",
        GridLayout::two_by_two(),
        vec![
            MultiviewSource::Input(input(1)),
            MultiviewSource::Input(input(2)),
            MultiviewSource::Input(input(3)),
            MultiviewSource::Input(input(4)),
        ],
        ConfidenceMode::InputsOnly,
        MultiviewOverlays::none()
            .with_labels(true)
            .with_tally(true)
            .with_clocks(false)
            .with_meters(true),
    )
    .unwrap();

    let program_plan = plan_multiview(2, 2, &program).unwrap();
    let quad_plan = plan_multiview(4, 4, &quad).unwrap();
    assert_eq!(program_plan.len(), 1);
    assert_eq!(quad_plan.len(), 4);
    assert_ne!(program_plan, quad_plan);
    assert_eq!(program_plan.cell(0).unwrap().width, 2);
    assert_eq!(quad_plan.cell(0).unwrap().width, 2);

    let colors = [
        Rgba8::new(1, 1, 1, 255),
        Rgba8::new(2, 2, 2, 255),
        Rgba8::new(3, 3, 3, 255),
        Rgba8::new(4, 4, 4, 255),
        Rgba8::new(5, 5, 5, 255),
    ];
    let frames = colors.map(|color| frame(1, 1, &[color]));
    let cpu_sources = [
        CpuSourceFrame::new(source(1), &frames[0]),
        CpuSourceFrame::new(source(2), &frames[1]),
        CpuSourceFrame::new(source(3), &frames[2]),
        CpuSourceFrame::new(source(4), &frames[3]),
        CpuSourceFrame::new(source(5), &frames[4]),
    ];
    let mut bindings = std::collections::HashMap::new();
    bindings.insert(MultiviewSource::Program, source(1));
    for (offset, value) in (1_u128..=4).enumerate() {
        bindings.insert(
            MultiviewSource::Input(input(value)),
            source(u64::try_from(offset).unwrap() + 2),
        );
    }
    let resolve = |cell: MultiviewSource| bindings.get(&cell).copied();

    let program_scene =
        compile_multiview_scene(&program_plan, 2, 2, BACKGROUND, 0, resolve).unwrap();
    let quad_scene = compile_multiview_scene(&quad_plan, 4, 4, BACKGROUND, 0, resolve).unwrap();
    assert_eq!(program_scene.width(), 2);
    assert_eq!(program_scene.height(), 2);
    assert_eq!(program_scene.layers().len(), 1);
    assert_eq!(quad_scene.width(), 4);
    assert_eq!(quad_scene.height(), 4);
    assert_eq!(quad_scene.layers().len(), 4);
    assert_ne!(program_scene, quad_scene);

    assert_eq!(
        pixels(&render(&program_scene, &cpu_sources)),
        vec![colors[0]; 4]
    );
    let expected = vec![
        colors[1], colors[1], colors[2], colors[2], colors[1], colors[1], colors[2], colors[2],
        colors[3], colors[3], colors[4], colors[4], colors[3], colors[3], colors[4], colors[4],
    ];
    assert_eq!(pixels(&render(&quad_scene, &cpu_sources)), expected);
}

#[test]
fn planning_and_compilation_are_deterministic() {
    let definition = quad_definition();
    let first = plan_multiview(5, 3, &definition).unwrap();
    let second = plan_multiview(5, 3, &definition).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.cells(), second.cells());

    let first_scene = compile_multiview_scene(&first, 5, 3, BACKGROUND, 1, resolve_quad).unwrap();
    let second_scene = compile_multiview_scene(&second, 5, 3, BACKGROUND, 1, resolve_quad).unwrap();
    assert_eq!(first_scene, second_scene);

    let colors = [
        Rgba8::new(9, 9, 9, 255),
        Rgba8::new(8, 8, 8, 255),
        Rgba8::new(7, 7, 7, 255),
        Rgba8::new(6, 6, 6, 255),
    ];
    let frames = colors.map(|color| frame(1, 1, &[color]));
    let cpu_sources = [
        CpuSourceFrame::new(source(1), &frames[0]),
        CpuSourceFrame::new(source(2), &frames[1]),
        CpuSourceFrame::new(source(3), &frames[2]),
        CpuSourceFrame::new(source(4), &frames[3]),
    ];
    let first_output = render(&first_scene, &cpu_sources);
    let second_output = render(&second_scene, &cpu_sources);
    assert_eq!(first_output.pixels(), second_output.pixels());
    assert_eq!(first_output.pixels().len(), 5 * 3 * 4);
}

#[test]
fn input_ids_preserve_the_full_128_bit_value() {
    let wide = input(u128::MAX);
    assert_eq!(wide.get().get(), u128::MAX);

    let definition = definition(
        "wide",
        GridLayout::one_by_one(),
        vec![MultiviewSource::Input(wide)],
    );
    let plan = plan_multiview(4, 4, &definition).unwrap();
    assert_eq!(plan.hit_test(1, 1), Some(MultiviewSource::Input(wide)));

    let scene = compile_multiview_scene(&plan, 4, 4, BACKGROUND, 0, |cell| {
        assert_eq!(cell, MultiviewSource::Input(wide));
        Some(source(1))
    })
    .unwrap();
    assert_eq!(scene.layers()[0].source(), source(1));
}

#[test]
fn overlay_flags_are_independent_placement_intent() {
    let overlays = MultiviewOverlays::none()
        .with_labels(true)
        .with_tally(true)
        .with_clocks(false)
        .with_meters(true);
    assert_eq!(
        overlays,
        MultiviewOverlays {
            labels: true,
            tally: true,
            clocks: false,
            meters: true,
        }
    );
    assert_eq!(
        MultiviewOverlays::ALL,
        MultiviewOverlays {
            labels: true,
            tally: true,
            clocks: true,
            meters: true,
        }
    );
    assert_eq!(MultiviewOverlays::NONE, MultiviewOverlays::none());
}
