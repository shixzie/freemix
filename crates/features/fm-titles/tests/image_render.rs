//! Decoded image rendering tests.
//!
//! These use the public API only: `ImageFrame`s are constructed by hand,
//! registered in an `AssetCatalog`, and rendered with `ReferenceRenderer`.

use fm_titles::{
    Alignment, AssetCatalog, Bounds, Color, Element, ElementId, ElementKind, FieldDefinition,
    FieldId, FieldValue, ImageAssetError, ImageFilter, ImageFit, ImageSource,
    MAX_IMAGE_ASSET_BYTES, MAX_IMAGE_ASSETS, MissingImage, ReferenceRenderer, Style, TemplateId,
    TitleId, TitleScene, TitleTemplate,
};
use fm_video::{ImageFrame, Rgba8};
use std::num::NonZeroU128;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn field_id(value: u128) -> FieldId {
    FieldId::new(NonZeroU128::new(value).unwrap())
}

fn element_id(value: u128) -> ElementId {
    ElementId::new(NonZeroU128::new(value).unwrap())
}

fn template_id(value: u128) -> TemplateId {
    TemplateId::new(NonZeroU128::new(value).unwrap())
}

fn title_id(value: u128) -> TitleId {
    TitleId::new(NonZeroU128::new(value).unwrap())
}

/// Packs row-major RGBA8 pixels into a packed frame.
fn rgba_frame(width: u32, height: u32, pixels: &[Rgba8]) -> ImageFrame {
    let width_usize = usize::try_from(width).unwrap();
    let height_usize = usize::try_from(height).unwrap();
    assert_eq!(pixels.len(), width_usize * height_usize);
    let mut bytes = Vec::with_capacity(pixels.len() * 4);
    for pixel in pixels {
        bytes.extend_from_slice(&pixel.to_bytes());
    }
    ImageFrame::new(width, height, width_usize * 4, bytes).unwrap()
}

fn plain_style() -> Style {
    Style {
        fill: Color::new(255, 255, 255, 255),
        background: None,
        opacity: 255,
        font: None,
        alignment: Alignment::default(),
    }
}

/// A style whose placeholder is easy to identify: black background, white mark.
fn placeholder_style() -> Style {
    Style {
        background: Some(Color::BLACK),
        ..plain_style()
    }
}

fn image_element(id: u128, z_index: i32, bounds: Bounds, style: Style) -> Element {
    Element {
        id: element_id(id),
        name: "image".into(),
        bounds,
        z_index,
        visible: true,
        style,
        color_field: None,
        kind: ElementKind::Image {
            field: field_id(1),
            fit: ImageFit::Stretch,
            filter: ImageFilter::Nearest,
        },
        animations: Vec::new(),
    }
}

fn rectangle(id: u128, z_index: i32, fill: Color, bounds: Bounds) -> Element {
    Element {
        id: element_id(id),
        name: format!("rectangle-{id}"),
        bounds,
        z_index,
        visible: true,
        style: Style {
            fill,
            ..plain_style()
        },
        color_field: None,
        kind: ElementKind::Rectangle,
        animations: Vec::new(),
    }
}

fn image_template(
    width: u32,
    height: u32,
    background: Color,
    asset: &str,
    elements: Vec<Element>,
) -> TitleTemplate {
    TitleTemplate {
        id: template_id(1),
        name: "image template".into(),
        width,
        height,
        background,
        fields: vec![FieldDefinition {
            id: field_id(1),
            name: "logo".into(),
            default: FieldValue::Image(ImageSource::new(asset)),
        }],
        elements,
    }
}

/// The placeholder's identifying pixels for a 4x4 element at the origin:
/// `(1, 0)` is off the diagonal cross and stays background; `(0, 0)` is the
/// mark. `placeholder_style` makes the two colours black and white.
fn assert_placeholder(frame: &ImageFrame, bounds: Bounds) {
    let x = u32::try_from(bounds.x).unwrap();
    let y = u32::try_from(bounds.y).unwrap();
    assert_eq!(
        frame.pixel(x + 1, y).unwrap().to_bytes(),
        [0, 0, 0, 255],
        "off the cross the placeholder background must show"
    );
    assert_eq!(
        frame.pixel(x, y).unwrap().to_bytes(),
        [255, 255, 255, 255],
        "the placeholder diagonal mark must start at the element origin"
    );
}

#[test]
fn two_by_two_source_stretches_into_exact_two_pixel_blocks() {
    let top_left = Rgba8::new(255, 0, 0, 255);
    let top_right = Rgba8::new(0, 255, 0, 255);
    let bottom_left = Rgba8::new(0, 0, 255, 255);
    let bottom_right = Rgba8::new(255, 255, 0, 255);
    let assets = AssetCatalog::new()
        .with_decoded_image(
            "logo",
            rgba_frame(2, 2, &[top_left, top_right, bottom_left, bottom_right]),
        )
        .expect("a 2x2 frame is within the asset cap");
    let scene = image_template(
        4,
        4,
        Color::TRANSPARENT,
        "logo",
        vec![image_element(
            1,
            0,
            Bounds {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            },
            plain_style(),
        )],
    )
    .instantiate(title_id(1))
    .expect("valid template");

    let output = ReferenceRenderer.render(&scene, 0, &assets).unwrap();
    let expected = [
        [top_left, top_left, top_right, top_right],
        [top_left, top_left, top_right, top_right],
        [bottom_left, bottom_left, bottom_right, bottom_right],
        [bottom_left, bottom_left, bottom_right, bottom_right],
    ];
    for (y, row) in expected.iter().enumerate() {
        for (x, pixel) in row.iter().enumerate() {
            let x = u32::try_from(x).unwrap();
            let y = u32::try_from(y).unwrap();
            assert_eq!(
                output.frame.pixel(x, y).unwrap().to_bytes(),
                pixel.to_bytes(),
                "pixel ({x}, {y})"
            );
        }
    }
    assert_eq!(output.report.missing_images, Vec::new());
}

#[test]
fn nondivisible_scaling_pins_floor_mapping() {
    // 3x3 source into 5x5 bounds. Destination index d samples floor(d * 3 / 5):
    // 0, 0, 1, 1, 2 on each axis, so the visible blocks are 2x2, 2x1, 1x2, 1x1.
    let mut pixels = Vec::new();
    for y in 0..3u32 {
        for x in 0..3u32 {
            pixels.push(Rgba8::new(
                u8::try_from(10 * x + 1).unwrap(),
                u8::try_from(10 * y + 1).unwrap(),
                0,
                255,
            ));
        }
    }
    let assets = AssetCatalog::new()
        .with_decoded_image("logo", rgba_frame(3, 3, &pixels))
        .expect("a 3x3 frame is within the asset cap");
    let scene = image_template(
        5,
        5,
        Color::TRANSPARENT,
        "logo",
        vec![image_element(
            1,
            0,
            Bounds {
                x: 0,
                y: 0,
                width: 5,
                height: 5,
            },
            plain_style(),
        )],
    )
    .instantiate(title_id(1))
    .expect("valid template");

    let output = ReferenceRenderer.render(&scene, 0, &assets).unwrap();
    let source_index = [0usize, 0, 1, 1, 2];
    for (destination_y, &source_y) in source_index.iter().enumerate() {
        for (destination_x, &source_x) in source_index.iter().enumerate() {
            let expected = pixels[source_y * 3 + source_x];
            let x = u32::try_from(destination_x).unwrap();
            let y = u32::try_from(destination_y).unwrap();
            assert_eq!(
                output.frame.pixel(x, y).unwrap().to_bytes(),
                expected.to_bytes(),
                "pixel ({x}, {y}) must sample source ({source_x}, {source_y})"
            );
        }
    }
}

#[test]
fn clipping_draws_only_the_canvas_intersection() {
    let top_left = Rgba8::new(255, 0, 0, 255);
    let top_right = Rgba8::new(0, 255, 0, 255);
    let bottom_left = Rgba8::new(0, 0, 255, 255);
    let bottom_right = Rgba8::new(255, 255, 0, 255);
    let assets = AssetCatalog::new()
        .with_decoded_image(
            "logo",
            rgba_frame(2, 2, &[top_left, top_right, bottom_left, bottom_right]),
        )
        .expect("a 2x2 frame is within the asset cap");

    // The element starts two pixels left of and one pixel above the canvas.
    // Canvas (0, 0) is element-local (2, 1), which maps to source (1, 0); the
    // element's right two columns and bottom row fall outside the canvas.
    let scene = image_template(
        4,
        4,
        Color::TRANSPARENT,
        "logo",
        vec![image_element(
            1,
            0,
            Bounds {
                x: -2,
                y: -1,
                width: 4,
                height: 4,
            },
            plain_style(),
        )],
    )
    .instantiate(title_id(1))
    .expect("valid template");
    let output = ReferenceRenderer.render(&scene, 0, &assets).unwrap();
    let expected = [
        [top_right, top_right],
        [bottom_right, bottom_right],
        [bottom_right, bottom_right],
    ];
    for (y, row) in expected.iter().enumerate() {
        for (x, pixel) in row.iter().enumerate() {
            let x = u32::try_from(x).unwrap();
            let y = u32::try_from(y).unwrap();
            assert_eq!(
                output.frame.pixel(x, y).unwrap().to_bytes(),
                pixel.to_bytes(),
                "clipped pixel ({x}, {y})"
            );
        }
    }
    for (x, y) in [
        (2, 0),
        (3, 0),
        (2, 1),
        (3, 1),
        (2, 2),
        (3, 2),
        (0, 3),
        (1, 3),
        (2, 3),
        (3, 3),
    ] {
        assert_eq!(
            output.frame.pixel(x, y).unwrap().to_bytes(),
            [0, 0, 0, 0],
            "pixel ({x}, {y}) outside the element must stay background"
        );
    }

    // Fully off-canvas to the right and below, and fully off to the top-left:
    // both must draw nothing and must not panic.
    for (id, bounds) in [
        (
            2,
            Bounds {
                x: 10,
                y: 10,
                width: 4,
                height: 4,
            },
        ),
        (
            3,
            Bounds {
                x: -10,
                y: -10,
                width: 4,
                height: 4,
            },
        ),
    ] {
        let scene = image_template(
            4,
            4,
            Color::TRANSPARENT,
            "logo",
            vec![image_element(id, 0, bounds, plain_style())],
        )
        .instantiate(title_id(id))
        .expect("valid template");
        let output = ReferenceRenderer.render(&scene, 0, &assets).unwrap();
        assert!(
            output.frame.pixels().iter().all(|byte| *byte == 0),
            "an element fully off-canvas must not ink anything"
        );
    }
}

#[test]
fn semi_transparent_source_pixel_composites_over_the_background() {
    // Background (10, 20, 30, 255) is opaque premultiplied. The source pixel is
    // premultiplied (100, 50, 25, 128) and the element is fully opaque, so:
    //   alpha = round(128 * 255 / 255) = 128, inverse = 127
    //   destination = source + round(destination * 127 / 255)
    //     r = 100 + round(10  * 127 / 255) = 100 + 5  = 105
    //     g =  50 + round(20  * 127 / 255) =  50 + 10 =  60
    //     b =  25 + round(30  * 127 / 255) =  25 + 15 =  40
    //     a = 128 + round(255 * 127 / 255) = 128 + 127 = 255
    let assets = AssetCatalog::new()
        .with_decoded_image("logo", rgba_frame(1, 1, &[Rgba8::new(100, 50, 25, 128)]))
        .expect("a 1x1 frame is within the asset cap");
    let scene = image_template(
        1,
        1,
        Color::new(10, 20, 30, 255),
        "logo",
        vec![image_element(
            1,
            0,
            Bounds {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            plain_style(),
        )],
    )
    .instantiate(title_id(1))
    .expect("valid template");

    let output = ReferenceRenderer.render(&scene, 0, &assets).unwrap();
    assert_eq!(
        output.frame.pixel(0, 0).unwrap().to_bytes(),
        [105, 60, 40, 255]
    );
}

#[test]
fn element_opacity_scales_decoded_pixels_like_the_source_over_rule() {
    // style.opacity 128 halves the coverage of opaque red (255, 0, 0, 255)
    // over (10, 20, 30, 255):
    //   r = round(255 * 128 / 255) + round(10 * 127 / 255) = 128 + 5  = 133
    //   g =         0               + round(20 * 127 / 255) =   0 + 10 =  10
    //   b =         0               + round(30 * 127 / 255) =   0 + 15 =  15
    //   a = round(255 * 128 / 255)  + round(255 * 127 / 255) = 128 + 127 = 255
    let assets = AssetCatalog::new()
        .with_decoded_image("logo", rgba_frame(1, 1, &[Rgba8::new(255, 0, 0, 255)]))
        .expect("a 1x1 frame is within the asset cap");
    let scene = image_template(
        1,
        1,
        Color::new(10, 20, 30, 255),
        "logo",
        vec![image_element(
            1,
            0,
            Bounds {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            Style {
                opacity: 128,
                ..plain_style()
            },
        )],
    )
    .instantiate(title_id(1))
    .expect("valid template");

    let output = ReferenceRenderer.render(&scene, 0, &assets).unwrap();
    assert_eq!(
        output.frame.pixel(0, 0).unwrap().to_bytes(),
        [133, 10, 15, 255]
    );
}

#[test]
fn unknown_name_only_and_decoded_assets_report_and_draw_distinctly() {
    let bounds = Bounds {
        x: 0,
        y: 0,
        width: 4,
        height: 4,
    };
    let scene = image_template(
        4,
        4,
        Color::TRANSPARENT,
        "logo",
        vec![image_element(1, 0, bounds, placeholder_style())],
    )
    .instantiate(title_id(1))
    .expect("valid template");

    // Never registered: the placeholder draws and the asset is reported.
    let output = ReferenceRenderer
        .render(&scene, 0, &AssetCatalog::new())
        .unwrap();
    assert_eq!(
        output.report.missing_images,
        vec![MissingImage {
            field: field_id(1),
            asset: "logo".into(),
        }]
    );
    assert_placeholder(&output.frame, bounds);

    // Name-only registration: still a placeholder, but not missing.
    let name_only = AssetCatalog::new().with_image("logo");
    assert!(name_only.has_image("logo"));
    assert!(name_only.image("logo").is_none());
    let output = ReferenceRenderer.render(&scene, 0, &name_only).unwrap();
    assert_eq!(output.report.missing_images, Vec::new());
    assert_placeholder(&output.frame, bounds);

    // Decoded pixels: no placeholder, no missing report.
    let red = Rgba8::new(255, 0, 0, 255);
    let decoded = AssetCatalog::new()
        .with_decoded_image("logo", rgba_frame(2, 2, &[red, red, red, red]))
        .expect("a 2x2 frame is within the asset cap");
    assert!(decoded.has_image("logo"));
    assert!(decoded.image("logo").is_some());
    let output = ReferenceRenderer.render(&scene, 0, &decoded).unwrap();
    assert_eq!(output.report.missing_images, Vec::new());
    for (index, pixel) in output.frame.pixels().as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(
            *pixel,
            red.to_bytes(),
            "pixel {index} must be decoded red, not a placeholder"
        );
    }
}

#[test]
fn builder_rejects_empty_names_and_oversized_frames() {
    let tiny = |value: u8| rgba_frame(1, 1, &[Rgba8::new(value, 0, 0, 255)]);
    assert_eq!(
        AssetCatalog::new()
            .with_decoded_image("", tiny(1))
            .unwrap_err(),
        ImageAssetError::EmptyName
    );
    assert_eq!(
        AssetCatalog::new()
            .with_decoded_image("   ", tiny(1))
            .unwrap_err(),
        ImageAssetError::EmptyName
    );

    // 2049x2048 RGBA8 is 16_785_408 bytes: just over the 16 MiB cap.
    let width = 2049u32;
    let height = 2048u32;
    let width_usize = usize::try_from(width).unwrap();
    let bytes = vec![0_u8; width_usize * usize::try_from(height).unwrap() * 4];
    let oversized = ImageFrame::new(width, height, width_usize * 4, bytes).unwrap();
    assert_eq!(oversized.pixels().len(), 16_785_408);
    assert_eq!(
        AssetCatalog::new()
            .with_decoded_image("huge", oversized)
            .unwrap_err(),
        ImageAssetError::FrameTooLarge {
            bytes: 16_785_408,
            maximum: MAX_IMAGE_ASSET_BYTES,
        }
    );

    // Exactly at the cap (2048x2048 RGBA8 = 16 MiB) is accepted.
    let side = 2048u32;
    let side_usize = usize::try_from(side).unwrap();
    let at_cap = ImageFrame::new(side, side, side_usize * 4, vec![0_u8; 16 * 1024 * 1024]).unwrap();
    assert_eq!(at_cap.pixels().len(), MAX_IMAGE_ASSET_BYTES);
    let catalog = AssetCatalog::new()
        .with_decoded_image("at-cap", at_cap)
        .expect("a frame exactly at the byte cap is accepted");
    assert!(catalog.image("at-cap").is_some());
}

#[test]
fn builder_caps_the_asset_count_and_replacement_does_not_grow_it() {
    let mut catalog = AssetCatalog::new();
    for index in 0..MAX_IMAGE_ASSETS {
        catalog = catalog
            .with_decoded_image(
                format!("asset-{index}"),
                rgba_frame(1, 1, &[Rgba8::new(u8::try_from(index).unwrap(), 0, 0, 255)]),
            )
            .unwrap_or_else(|error| panic!("asset {index} is within the cap: {error}"));
    }
    // The catalog is full: one more name is refused.
    let error = catalog
        .with_decoded_image(
            "one-too-many",
            rgba_frame(1, 1, &[Rgba8::new(0, 0, 0, 255)]),
        )
        .unwrap_err();
    assert_eq!(
        error,
        ImageAssetError::TooManyAssets {
            maximum: MAX_IMAGE_ASSETS,
        }
    );

    // Replacing an existing name with decoded pixels succeeds without growing
    // the count, even at the cap, and the pixels are available afterwards.
    let mut full = AssetCatalog::new();
    for index in 0..MAX_IMAGE_ASSETS {
        full = full.with_image(format!("name-{index}"));
    }
    let green = Rgba8::new(0, 255, 0, 255);
    let full = full
        .with_decoded_image("name-0", rgba_frame(1, 1, &[green]))
        .expect("replacing an existing name never exceeds the cap");
    assert_eq!(
        full.image("name-0")
            .unwrap()
            .pixel(0, 0)
            .unwrap()
            .to_bytes(),
        green.to_bytes()
    );
    assert!(full.has_image("name-0"));
    assert_eq!(
        full.with_decoded_image("name-new", rgba_frame(1, 1, &[green]))
            .unwrap_err(),
        ImageAssetError::TooManyAssets {
            maximum: MAX_IMAGE_ASSETS,
        },
        "replacement did not grow the catalog, so a new name is still refused"
    );

    // Replacing an already decoded entry swaps the pixels under the same name.
    let catalog = AssetCatalog::new()
        .with_decoded_image("logo", rgba_frame(1, 1, &[Rgba8::new(255, 0, 0, 255)]))
        .unwrap()
        .with_decoded_image("logo", rgba_frame(1, 1, &[green]))
        .unwrap();
    assert_eq!(
        catalog
            .image("logo")
            .unwrap()
            .pixel(0, 0)
            .unwrap()
            .to_bytes(),
        green.to_bytes()
    );
    assert!(catalog.has_image("logo"));
}

fn full_bounds() -> Bounds {
    Bounds {
        x: 0,
        y: 0,
        width: 4,
        height: 4,
    }
}

fn right_half() -> Bounds {
    Bounds {
        x: 2,
        y: 0,
        width: 2,
        height: 4,
    }
}

fn four_by_four(elements: Vec<Element>) -> TitleScene {
    image_template(4, 4, Color::TRANSPARENT, "logo", elements)
        .instantiate(title_id(1))
        .expect("valid template")
}

fn decoded_red_assets() -> AssetCatalog {
    let red = Rgba8::new(255, 0, 0, 255);
    AssetCatalog::new()
        .with_decoded_image("logo", rgba_frame(2, 2, &[red, red, red, red]))
        .expect("a 2x2 frame is within the asset cap")
}

fn output_pixel(frame: &ImageFrame, x: u32, y: u32) -> [u8; 4] {
    frame.pixel(x, y).unwrap().to_bytes()
}

#[test]
fn image_and_rectangle_z_order_is_exact_and_deterministic() {
    let assets = decoded_red_assets();
    let elements = vec![
        image_element(1, 0, full_bounds(), plain_style()),
        rectangle(2, 1, Color::new(0, 0, 255, 255), right_half()),
    ];
    // The rectangle's z-index 1 puts it above the image's z-index 0, so the
    // right half is blue and the left half is the decoded red.
    let scene = four_by_four(elements.clone());
    let first = ReferenceRenderer.render(&scene, 0, &assets).unwrap();
    let second = ReferenceRenderer.render(&scene, 0, &assets).unwrap();
    assert_eq!(
        first.frame.pixels(),
        second.frame.pixels(),
        "identical input must render byte-identical output"
    );
    let reinstantiated = four_by_four(elements);
    assert_eq!(
        first.frame.pixels(),
        ReferenceRenderer
            .render(&reinstantiated, 0, &assets)
            .unwrap()
            .frame
            .pixels(),
        "a separately instantiated identical scene must rasterize identically"
    );
    for y in 0..4u32 {
        for x in 0..4u32 {
            let expected = if x < 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            assert_eq!(
                output_pixel(&first.frame, x, y),
                expected,
                "pixel ({x}, {y})"
            );
        }
    }
}

#[test]
fn equal_z_index_draws_the_later_element_on_top() {
    let assets = decoded_red_assets();
    let scene = four_by_four(vec![
        rectangle(2, 0, Color::new(0, 0, 255, 255), right_half()),
        image_element(1, 0, full_bounds(), plain_style()),
    ]);
    let output = ReferenceRenderer.render(&scene, 0, &assets).unwrap();
    for y in 0..4u32 {
        for x in 0..4u32 {
            assert_eq!(
                output_pixel(&output.frame, x, y),
                [255, 0, 0, 255],
                "the later image element must cover the rectangle at ({x}, {y})"
            );
        }
    }
}

#[test]
fn hidden_elements_draw_nothing() {
    let assets = decoded_red_assets();

    // A hidden rectangle above the image cannot cover it.
    let mut hidden_rectangle = rectangle(2, 1, Color::new(0, 0, 255, 255), right_half());
    hidden_rectangle.visible = false;
    let scene = four_by_four(vec![
        image_element(1, 0, full_bounds(), plain_style()),
        hidden_rectangle,
    ]);
    let output = ReferenceRenderer.render(&scene, 0, &assets).unwrap();
    for y in 0..4u32 {
        for x in 0..4u32 {
            assert_eq!(
                output_pixel(&output.frame, x, y),
                [255, 0, 0, 255],
                "a hidden rectangle must not cover the image at ({x}, {y})"
            );
        }
    }

    // A hidden image above the rectangle cannot cover it either.
    let mut hidden_image = image_element(1, 1, full_bounds(), plain_style());
    hidden_image.visible = false;
    let scene = four_by_four(vec![
        rectangle(2, 0, Color::new(0, 0, 255, 255), full_bounds()),
        hidden_image,
    ]);
    let output = ReferenceRenderer.render(&scene, 0, &assets).unwrap();
    for y in 0..4u32 {
        for x in 0..4u32 {
            assert_eq!(
                output_pixel(&output.frame, x, y),
                [0, 0, 255, 255],
                "a hidden image must not cover the rectangle at ({x}, {y})"
            );
        }
    }
}

#[test]
fn a_large_source_and_element_cost_only_the_canvas() {
    // 2048x2048 RGBA8 is exactly the 16 MiB asset cap, and the element is far
    // larger than the canvas: iterating the element's declared extent would
    // visit 3.6e9 pixels and scaling the source wholesale would touch 4.2e6
    // pixels per frame. Only the canvas intersection may be visited.
    let side = 2048u32;
    let side_usize = usize::try_from(side).unwrap();
    let stride = side_usize * 4;
    let mut bytes = vec![0_u8; stride * side_usize];
    for pixel in bytes.as_chunks_mut::<4>().0 {
        *pixel = [9, 8, 7, 255];
    }
    let source = ImageFrame::new(side, side, stride, bytes).unwrap();
    assert_eq!(source.pixels().len(), MAX_IMAGE_ASSET_BYTES);
    let assets = AssetCatalog::new()
        .with_decoded_image("logo", source)
        .expect("the frame is exactly at the byte cap");
    let scene = image_template(
        8,
        8,
        Color::TRANSPARENT,
        "logo",
        vec![image_element(
            1,
            0,
            Bounds {
                x: 0,
                y: 0,
                width: 60_000,
                height: 60_000,
            },
            plain_style(),
        )],
    )
    .instantiate(title_id(1))
    .expect("valid template");

    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let output = ReferenceRenderer
            .render(&scene, 0, &assets)
            .expect("frame renders");
        sender.send(output.frame.pixels().to_vec()).ok();
    });
    let pixels = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("image work must be bounded by the canvas, not by the element extent");
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| *pixel == [9, 8, 7, 255]),
        "the clipped image must still sample its source over the whole canvas"
    );
}
