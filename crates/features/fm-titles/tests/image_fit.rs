//! Image fit modes, alignment, and filtering.
//!
//! Every expectation is an exact pixel derived by hand from the documented
//! integer placement (`round(other_source * bound / source)`, floor-centered
//! alignment) and the 8-bit fixed-point bilinear rule.

use fm_titles::{
    Alignment, AssetCatalog, Bounds, Color, Element, ElementId, ElementKind, FieldDefinition,
    FieldId, FieldValue, HorizontalAlignment, ImageFilter, ImageFit, ImageSource,
    ReferenceRenderer, Style, TemplateId, TitleId, TitleScene, TitleTemplate, VerticalAlignment,
};
use fm_video::ImageFrame;
use std::num::NonZeroU128;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const R: [u8; 4] = [255, 0, 0, 255];
const G: [u8; 4] = [0, 255, 0, 255];
const B: [u8; 4] = [0, 0, 255, 255];
const Y: [u8; 4] = [255, 255, 0, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn frame(width: u32, height: u32, pixels: &[[u8; 4]]) -> ImageFrame {
    let width_usize = usize::try_from(width).unwrap();
    assert_eq!(pixels.len(), width_usize * usize::try_from(height).unwrap());
    ImageFrame::new(width, height, width_usize * 4, pixels.concat()).unwrap()
}

fn align(horizontal: HorizontalAlignment, vertical: VerticalAlignment) -> Alignment {
    Alignment {
        horizontal,
        vertical,
    }
}

fn centered() -> Alignment {
    align(HorizontalAlignment::Center, VerticalAlignment::Center)
}

struct Case {
    canvas: (u32, u32),
    bounds: Bounds,
    fit: ImageFit,
    filter: ImageFilter,
    alignment: Alignment,
}

impl Case {
    fn new(canvas: (u32, u32), bounds: Bounds, fit: ImageFit, alignment: Alignment) -> Self {
        Self {
            canvas,
            bounds,
            fit,
            filter: ImageFilter::Nearest,
            alignment,
        }
    }

    fn filter(mut self, filter: ImageFilter) -> Self {
        self.filter = filter;
        self
    }

    fn scene(&self) -> TitleScene {
        let field = FieldId::new(id(1));
        TitleTemplate {
            id: TemplateId::new(id(1)),
            name: "fit".into(),
            width: self.canvas.0,
            height: self.canvas.1,
            background: Color::TRANSPARENT,
            fields: vec![FieldDefinition {
                id: field,
                name: "logo".into(),
                default: FieldValue::Image(ImageSource::new("logo")),
            }],
            elements: vec![Element {
                id: ElementId::new(id(1)),
                name: "image".into(),
                bounds: self.bounds,
                z_index: 0,
                visible: true,
                style: Style {
                    alignment: self.alignment,
                    ..Style::default()
                },
                color_field: None,
                kind: ElementKind::Image {
                    field,
                    fit: self.fit,
                    filter: self.filter,
                },
            }],
            animation: fm_titles::TitleAnimation::default(),
        }
        .instantiate(TitleId::new(id(1)))
        .expect("valid template")
    }

    fn render(&self, source: ImageFrame) -> ImageFrame {
        let assets = AssetCatalog::new()
            .with_decoded_image("logo", source)
            .expect("small frame");
        let output = ReferenceRenderer
            .render(&self.scene(), 0, &assets)
            .expect("frame renders");
        assert_eq!(output.report.missing_images, Vec::new());
        output.frame
    }
}

fn bounds(x: i32, y: i32, width: u32, height: u32) -> Bounds {
    Bounds {
        x,
        y,
        width,
        height,
    }
}

fn assert_rows(frame: &ImageFrame, expected: &[&[[u8; 4]]]) {
    assert_eq!(usize::try_from(frame.height()).unwrap(), expected.len());
    for (y, row) in expected.iter().enumerate() {
        assert_eq!(usize::try_from(frame.width()).unwrap(), row.len());
        for (x, pixel) in row.iter().enumerate() {
            let x = u32::try_from(x).unwrap();
            let y = u32::try_from(y).unwrap();
            assert_eq!(
                frame.pixel(x, y).unwrap().to_bytes(),
                *pixel,
                "pixel ({x}, {y})"
            );
        }
    }
}

fn gray(value: u8) -> [u8; 4] {
    [value, value, value, 255]
}

#[test]
fn contain_letterboxes_and_aligns_vertically() {
    // 2x1 into 4x4: width-limited, placed 4x round(1 * 4 / 2) = 4x2.
    let source = || frame(2, 1, &[R, G]);
    let image_row: &[[u8; 4]] = &[R, R, G, G];
    let empty: &[[u8; 4]] = &[CLEAR; 4];
    let full = bounds(0, 0, 4, 4);

    let center = Case::new((4, 4), full, ImageFit::Contain, centered()).render(source());
    assert_rows(&center, &[empty, image_row, image_row, empty]);

    let top = Case::new(
        (4, 4),
        full,
        ImageFit::Contain,
        align(HorizontalAlignment::Center, VerticalAlignment::Start),
    )
    .render(source());
    assert_rows(&top, &[image_row, image_row, empty, empty]);

    let bottom = Case::new(
        (4, 4),
        full,
        ImageFit::Contain,
        align(HorizontalAlignment::Center, VerticalAlignment::End),
    )
    .render(source());
    assert_rows(&bottom, &[empty, empty, image_row, image_row]);
}

#[test]
fn contain_pillarboxes_and_floors_odd_centering() {
    // 1x2 into 5x4: height-limited, placed round(1 * 4 / 2) = 2 wide x 4 tall.
    // Centering offset is floor((5 - 2) / 2) = 1, so columns 1 and 2 draw.
    let output = Case::new((5, 4), bounds(0, 0, 5, 4), ImageFit::Contain, centered())
        .render(frame(1, 2, &[R, B]));
    assert_rows(
        &output,
        &[
            &[CLEAR, R, R, CLEAR, CLEAR],
            &[CLEAR, R, R, CLEAR, CLEAR],
            &[CLEAR, B, B, CLEAR, CLEAR],
            &[CLEAR, B, B, CLEAR, CLEAR],
        ],
    );
}

#[test]
fn contain_rounds_the_free_axis_half_up() {
    // 3x2 into 4x4: width-limited, height = round(2 * 4 / 3) = round(2.67) = 3.
    // Start alignment draws rows 0..3; row 3 stays clear. Nearest rows map
    // floor(d * 2 / 3) = 0, 0, 1 and columns floor(d * 3 / 4) = 0, 0, 1, 2.
    let output = Case::new(
        (4, 4),
        bounds(0, 0, 4, 4),
        ImageFit::Contain,
        Alignment::default(),
    )
    .render(frame(3, 2, &[R, G, B, Y, R, G]));
    assert_rows(
        &output,
        &[&[R, R, G, B], &[R, R, G, B], &[Y, Y, R, G], &[CLEAR; 4]],
    );
}

#[test]
fn cover_crops_overflow_by_alignment() {
    // 4x1 into 4x4: height-limited for cover, placed round(4 * 4 / 1) = 16 x 4.
    // Each source column spans four destination columns.
    let source = || frame(4, 1, &[R, G, B, Y]);
    let full = bounds(0, 0, 4, 4);
    let render = |horizontal| {
        Case::new(
            (4, 4),
            full,
            ImageFit::Cover,
            align(horizontal, VerticalAlignment::Center),
        )
        .render(source())
    };

    // Center offset floor((4 - 16) / 2) = -6: local columns 6..10 -> G G B B.
    let row: &[[u8; 4]] = &[G, G, B, B];
    assert_rows(&render(HorizontalAlignment::Center), &[row, row, row, row]);
    // Start shows local columns 0..4, all from source column 0.
    let row: &[[u8; 4]] = &[R; 4];
    assert_rows(&render(HorizontalAlignment::Start), &[row, row, row, row]);
    // End offset 4 - 16 = -12 shows local columns 12..16, all source column 3.
    let row: &[[u8; 4]] = &[Y; 4];
    assert_rows(&render(HorizontalAlignment::End), &[row, row, row, row]);
}

#[test]
fn cover_never_draws_outside_the_element_bounds() {
    // 1x1 source covering a 2x2 element at (1, 1) in a 4x4 canvas.
    let output = Case::new((4, 4), bounds(1, 1, 2, 2), ImageFit::Cover, centered()).render(frame(
        1,
        1,
        &[B],
    ));
    assert_rows(
        &output,
        &[
            &[CLEAR; 4],
            &[CLEAR, B, B, CLEAR],
            &[CLEAR, B, B, CLEAR],
            &[CLEAR; 4],
        ],
    );
}

#[test]
fn none_draws_native_size_aligned_and_cropped_to_the_element() {
    // 2x2 source at native size centered in a 4x4 element: offset 1.
    let source = || frame(2, 2, &[R, G, B, Y]);
    let output = Case::new((4, 4), bounds(0, 0, 4, 4), ImageFit::None, centered()).render(source());
    assert_rows(
        &output,
        &[
            &[CLEAR; 4],
            &[CLEAR, R, G, CLEAR],
            &[CLEAR, B, Y, CLEAR],
            &[CLEAR; 4],
        ],
    );

    // End/End in a 4x4 element places the image at (2, 2).
    let output = Case::new(
        (4, 4),
        bounds(0, 0, 4, 4),
        ImageFit::None,
        align(HorizontalAlignment::End, VerticalAlignment::End),
    )
    .render(source());
    assert_rows(
        &output,
        &[
            &[CLEAR; 4],
            &[CLEAR; 4],
            &[CLEAR, CLEAR, R, G],
            &[CLEAR, CLEAR, B, Y],
        ],
    );

    // A 1x1 element shows exactly one source pixel and nothing outside it.
    // Center offset floor((1 - 2) / 2) = -1 selects source (1, 1).
    let output = Case::new((3, 3), bounds(1, 1, 1, 1), ImageFit::None, centered()).render(source());
    assert_rows(&output, &[&[CLEAR; 3], &[CLEAR, Y, CLEAR], &[CLEAR; 3]]);
    // Start selects source (0, 0).
    let output = Case::new(
        (3, 3),
        bounds(1, 1, 1, 1),
        ImageFit::None,
        Alignment::default(),
    )
    .render(source());
    assert_rows(&output, &[&[CLEAR; 3], &[CLEAR, R, CLEAR], &[CLEAR; 3]]);
}

#[test]
fn stretch_ignores_alignment() {
    let output = Case::new(
        (4, 2),
        bounds(0, 0, 4, 2),
        ImageFit::Stretch,
        align(HorizontalAlignment::End, VerticalAlignment::End),
    )
    .render(frame(2, 1, &[R, G]));
    assert_rows(&output, &[&[R, R, G, G], &[R, R, G, G]]);
}

#[test]
fn bilinear_upscale_uses_pixel_centers_and_clamps_edges() {
    // Source x for destination d is (d + 0.5) * 2 / 4 - 0.5 =
    // -0.25, 0.25, 0.75, 1.25. Clamp-to-edge pins the ends to 0 and 255; the
    // interior is 255 * 64 / 256 = 63.75 -> 64 and 255 * 192 / 256 = 191.25 -> 191.
    let output = Case::new(
        (4, 1),
        bounds(0, 0, 4, 1),
        ImageFit::Stretch,
        Alignment::default(),
    )
    .filter(ImageFilter::Bilinear)
    .render(frame(2, 1, &[gray(0), gray(255)]));
    assert_rows(&output, &[&[gray(0), gray(64), gray(191), gray(255)]]);
}

#[test]
fn bilinear_interpolates_both_axes() {
    // Checkerboard 0/255 in red, upscaled 2x2 -> 4x4. Per-axis weights are
    // (pinned 0), 0.25, 0.75, (pinned 1). For example (1, 1) is
    // 255 * (0.25 * 0.75 + 0.75 * 0.25) = 95.625 -> 96 and (2, 1) is
    // 255 * (0.75 * 0.75 + 0.25 * 0.25) = 159.375 -> 159.
    let red = |value: u8| [value, 0, 0, 255];
    let output = Case::new(
        (4, 4),
        bounds(0, 0, 4, 4),
        ImageFit::Stretch,
        Alignment::default(),
    )
    .filter(ImageFilter::Bilinear)
    .render(frame(2, 2, &[red(0), red(255), red(255), red(0)]));
    assert_rows(
        &output,
        &[
            &[red(0), red(64), red(191), red(255)],
            &[red(64), red(96), red(159), red(191)],
            &[red(191), red(159), red(96), red(64)],
            &[red(255), red(191), red(64), red(0)],
        ],
    );
}

#[test]
fn bilinear_interpolates_premultiplied_alpha() {
    // Transparent black next to premultiplied half red. Interpolating the
    // premultiplied channels keeps colour and alpha in step: 128 * 0.25 = 32,
    // 128 * 0.75 = 96. Over a transparent background the result is exact.
    let output = Case::new(
        (4, 1),
        bounds(0, 0, 4, 1),
        ImageFit::Stretch,
        Alignment::default(),
    )
    .filter(ImageFilter::Bilinear)
    .render(frame(2, 1, &[[0, 0, 0, 0], [128, 0, 0, 128]]));
    assert_rows(
        &output,
        &[&[
            [0, 0, 0, 0],
            [32, 0, 0, 32],
            [96, 0, 0, 96],
            [128, 0, 0, 128],
        ]],
    );
}

#[test]
fn bilinear_at_native_size_is_the_identity() {
    let pixels = [R, G, B, Y, gray(7), [10, 20, 30, 40]];
    for fit in [
        ImageFit::None,
        ImageFit::Stretch,
        ImageFit::Contain,
        ImageFit::Cover,
    ] {
        let output = Case::new((3, 2), bounds(0, 0, 3, 2), fit, centered())
            .filter(ImageFilter::Bilinear)
            .render(frame(3, 2, &pixels));
        assert_eq!(output.pixels(), pixels.concat(), "{fit:?}");
    }
}

#[test]
fn bilinear_contain_combines_placement_and_filtering() {
    // 2x1 gray ramp contained in a 4x4 element: placed 4x2 at row 1. Each
    // placed row is the same 0, 64, 191, 255 ramp; rows 0 and 3 stay clear.
    let output = Case::new((4, 4), bounds(0, 0, 4, 4), ImageFit::Contain, centered())
        .filter(ImageFilter::Bilinear)
        .render(frame(2, 1, &[gray(0), gray(255)]));
    let ramp: &[[u8; 4]] = &[gray(0), gray(64), gray(191), gray(255)];
    assert_rows(&output, &[&[CLEAR; 4], ramp, ramp, &[CLEAR; 4]]);
}

#[test]
fn identical_inputs_render_identically_for_every_mode() {
    let pixels = [R, G, B, Y, gray(9), [1, 2, 3, 4]];
    for fit in [
        ImageFit::None,
        ImageFit::Stretch,
        ImageFit::Contain,
        ImageFit::Cover,
    ] {
        for filter in [ImageFilter::Nearest, ImageFilter::Bilinear] {
            let case = Case::new((7, 5), bounds(-1, 1, 6, 5), fit, centered()).filter(filter);
            let first = case.render(frame(3, 2, &pixels));
            let second = case.render(frame(3, 2, &pixels));
            assert_eq!(first.pixels(), second.pixels(), "{fit:?} {filter:?}");
        }
    }
}

#[test]
fn huge_cover_placement_costs_only_the_canvas() {
    // A 1x2048 strip covering a 60000x60000 element is placed 60000 x
    // 122_880_000 px. Only the 8x8 canvas intersection may be visited.
    let mut pixels = Vec::with_capacity(2048);
    for row in 0..2048_u32 {
        pixels.push(if row < 1024 { R } else { B });
    }
    let case = Case::new(
        (8, 8),
        bounds(0, 0, 60_000, 60_000),
        ImageFit::Cover,
        align(HorizontalAlignment::Center, VerticalAlignment::Start),
    )
    .filter(ImageFilter::Bilinear);
    let scene = case.scene();
    let assets = AssetCatalog::new()
        .with_decoded_image("logo", frame(1, 2048, &pixels))
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let output = ReferenceRenderer.render(&scene, 0, &assets).unwrap();
        sender.send(output.frame.pixels().to_vec()).ok();
    });
    let rendered = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("image work must be bounded by the canvas");
    // Start alignment crops everything below the strip's top 8 of 122_880_000
    // placed rows, whose pixel centers all clamp to the first (red) source row.
    assert!(rendered.as_chunks::<4>().0.iter().all(|pixel| *pixel == R));
}
