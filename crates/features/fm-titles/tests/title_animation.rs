//! Titles sampled through the unified `fm_animation` model: exact pixels for
//! position, scale, crop, color, opacity, and rotation at a halfway key.

use fm_titles::fm_animation::{
    AnimationPreset, ClipPosition, ClipState, Easing, Keyframe, Phase, Property, Rgba, Track,
    TrackKey, Value,
};
use fm_titles::{
    AnimationEditError, AssetCatalog, Bounds, Color, Degradation, DegradedElement, Element,
    ElementId, ElementKind, FieldDefinition, FieldId, FieldValue, FontStyle, ImageFilter, ImageFit,
    ImageSource, MAX_FONT_SIZE_PX, MAX_ROTATED_ELEMENT_PIXELS, ReferenceRenderer, Style,
    TemplateId, TitleAnimation, TitleId, TitleTemplate,
};
use fm_video::{ImageFrame, Rgba8};
use std::fs;
use std::num::NonZeroU128;

const RED: Color = Color::new(255, 0, 0, 255);
const SIZE: u32 = 16;
/// The halfway frame of a ten-frame In phase.
const HALF: u32 = 5;

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn element_id(value: u128) -> ElementId {
    ElementId::new(nonzero(value))
}

fn bounds(x: i32, y: i32, width: u32, height: u32) -> Bounds {
    Bounds {
        x,
        y,
        width,
        height,
    }
}

fn rectangle(area: Bounds) -> Element {
    Element {
        id: element_id(1),
        name: "box".into(),
        bounds: area,
        z_index: 0,
        visible: true,
        style: Style {
            fill: RED,
            ..Style::default()
        },
        color_field: None,
        kind: ElementKind::Rectangle,
    }
}

fn template(elements: Vec<Element>, fields: Vec<FieldDefinition>) -> TitleTemplate {
    TitleTemplate {
        id: TemplateId::new(nonzero(1)),
        name: "animated".into(),
        width: SIZE,
        height: SIZE,
        background: Color::TRANSPARENT,
        fields,
        elements,
        animation: TitleAnimation::default(),
    }
}

/// Adds a linear In track on element 1 from `from` at frame 0 to `to` at
/// frame 10, growing the In phase to ten frames.
fn animate(template: &mut TitleTemplate, property: Property, from: Value, to: Value) {
    template
        .edit_animation(|clip| {
            clip.set_phase_frames_keeping_keys(Phase::In, 10)?;
            clip.set_track(Track::new(
                TrackKey::new(element_id(1), Phase::In, property),
                vec![
                    Keyframe::new(0, from, Easing::Linear),
                    Keyframe::new(10, to, Easing::Linear),
                ],
            ))
        })
        .unwrap();
}

fn render(template: &TitleTemplate, sample: ClipState, assets: &AssetCatalog) -> ImageFrame {
    let scene = template.instantiate(TitleId::new(nonzero(1))).unwrap();
    let output = ReferenceRenderer
        .render_sampled(&scene, 0, assets, sample)
        .unwrap();
    assert!(output.report.degraded.is_empty(), "{:?}", output.report);
    output.frame
}

fn at(frame: u32) -> ClipState {
    ClipState::In(ClipPosition::frame(frame))
}

/// Asserts every pixel: `inside` where `(x, y)` is in `area`, transparent
/// elsewhere.
fn assert_box(frame: &ImageFrame, area: Bounds, inside: [u8; 4]) {
    for y in 0..SIZE {
        for x in 0..SIZE {
            let within = i64::from(x) >= i64::from(area.x)
                && i64::from(x) < i64::from(area.x) + i64::from(area.width)
                && i64::from(y) >= i64::from(area.y)
                && i64::from(y) < i64::from(area.y) + i64::from(area.height);
            let expected = if within { inside } else { [0; 4] };
            assert_eq!(
                frame.pixel(x, y).unwrap().to_bytes(),
                expected,
                "pixel ({x}, {y})"
            );
        }
    }
}

#[test]
fn position_offsets_the_box_at_the_halfway_key() {
    let mut title = template(vec![rectangle(bounds(4, 4, 8, 8))], Vec::new());
    animate(
        &mut title,
        Property::PositionX,
        Value::Scalar(-8),
        Value::Scalar(0),
    );
    animate(
        &mut title,
        Property::PositionY,
        Value::Scalar(0),
        Value::Scalar(4),
    );
    let assets = AssetCatalog::new();
    assert_box(
        &render(&title, at(HALF), &assets),
        bounds(0, 6, 8, 8),
        [255, 0, 0, 255],
    );
    // Static ignores the clip; Hold is the In phase's end.
    assert_box(
        &render(&title, ClipState::Static, &assets),
        bounds(4, 4, 8, 8),
        [255, 0, 0, 255],
    );
    assert_box(
        &render(&title, ClipState::Hold, &assets),
        bounds(4, 8, 8, 8),
        [255, 0, 0, 255],
    );
}

#[test]
fn scale_shrinks_the_box_about_its_centre_at_the_halfway_key() {
    let mut title = template(vec![rectangle(bounds(4, 4, 8, 8))], Vec::new());
    for property in [Property::ScaleX, Property::ScaleY] {
        animate(
            &mut title,
            property,
            Value::Scalar(0),
            Value::Scalar(10_000),
        );
    }
    let assets = AssetCatalog::new();
    // 8 * 0.5 = 4, centred: (4 + 2, 4 + 2).
    assert_box(
        &render(&title, at(HALF), &assets),
        bounds(6, 6, 4, 4),
        [255, 0, 0, 255],
    );
    // Frame 0 scales to nothing.
    assert_box(&render(&title, at(0), &assets), bounds(0, 0, 0, 0), [0; 4]);
    assert_box(
        &render(&title, ClipState::Hold, &assets),
        bounds(4, 4, 8, 8),
        [255, 0, 0, 255],
    );

    // An odd change puts the extra pixel on the right/bottom: 7 * 0.5 = 3.5
    // rounds to 4, and floor((7 - 4) / 2) = 1.
    let mut odd = template(vec![rectangle(bounds(4, 4, 7, 7))], Vec::new());
    for property in [Property::ScaleX, Property::ScaleY] {
        animate(&mut odd, property, Value::Scalar(0), Value::Scalar(10_000));
    }
    assert_box(
        &render(&odd, at(HALF), &assets),
        bounds(5, 5, 4, 4),
        [255, 0, 0, 255],
    );
}

#[test]
fn crop_insets_the_box_at_the_halfway_key() {
    let mut title = template(vec![rectangle(bounds(4, 4, 8, 8))], Vec::new());
    animate(
        &mut title,
        Property::CropLeft,
        Value::Scalar(0),
        Value::Scalar(8),
    );
    animate(
        &mut title,
        Property::CropBottom,
        Value::Scalar(4),
        Value::Scalar(0),
    );
    let assets = AssetCatalog::new();
    // Left crop 4, bottom crop 2.
    assert_box(
        &render(&title, at(HALF), &assets),
        bounds(8, 4, 4, 6),
        [255, 0, 0, 255],
    );
    // Cropping the whole width draws nothing.
    assert_box(
        &render(&title, ClipState::Hold, &assets),
        bounds(0, 0, 0, 0),
        [0; 4],
    );
}

#[test]
fn color_overrides_the_fill_at_the_halfway_key() {
    let mut title = template(vec![rectangle(bounds(4, 4, 8, 8))], Vec::new());
    animate(
        &mut title,
        Property::Color,
        Value::Color(Rgba::new(255, 0, 0, 255)),
        Value::Color(Rgba::new(0, 0, 255, 255)),
    );
    // Each channel: round_half_up(from + (to - from) * 0.5).
    assert_box(
        &render(&title, at(HALF), &AssetCatalog::new()),
        bounds(4, 4, 8, 8),
        [128, 0, 128, 255],
    );
}

#[test]
fn opacity_multiplies_into_the_static_opacity_at_the_halfway_key() {
    let mut title = template(vec![rectangle(bounds(4, 4, 8, 8))], Vec::new());
    animate(
        &mut title,
        Property::Opacity,
        Value::Scalar(0),
        Value::Scalar(255),
    );
    // 255 * 0.5 truncates to 127; premultiplied red at alpha 127.
    assert_box(
        &render(&title, at(HALF), &AssetCatalog::new()),
        bounds(4, 4, 8, 8),
        [127, 0, 0, 127],
    );

    // With a static opacity of 128 the two factors round once.
    title.elements[0].style.opacity = 128;
    // round(128 * 127 / 255) = 64.
    assert_box(
        &render(&title, at(HALF), &AssetCatalog::new()),
        bounds(4, 4, 8, 8),
        [64, 0, 0, 64],
    );
}

fn patterned_image(width: u32, height: u32) -> ImageFrame {
    let mut pixels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let r = u8::try_from(16 * x + 1).unwrap();
            let g = u8::try_from(32 * y + 3).unwrap();
            pixels.extend_from_slice(&[r, g, 7, 255]);
        }
    }
    ImageFrame::new(width, height, usize::try_from(width).unwrap() * 4, pixels).unwrap()
}

fn image_title(area: Bounds) -> (TitleTemplate, AssetCatalog) {
    let field = FieldId::new(nonzero(1));
    let element = Element {
        id: element_id(1),
        name: "image".into(),
        bounds: area,
        z_index: 0,
        visible: true,
        style: Style::default(),
        color_field: None,
        kind: ElementKind::Image {
            field,
            fit: ImageFit::None,
            filter: ImageFilter::Nearest,
        },
    };
    let title = template(
        vec![element],
        vec![FieldDefinition {
            id: field,
            name: "picture".into(),
            default: FieldValue::Image(ImageSource::new("picture")),
        }],
    );
    let assets = AssetCatalog::new()
        .with_decoded_image("picture", patterned_image(area.width, area.height))
        .unwrap();
    (title, assets)
}

/// The quarter-turn reference: every canvas pixel takes the unrotated pixel
/// its centre maps back to under a clockwise quarter turn about the box
/// centre, `(dx, dy) -> (dy, -dx)`.
fn quarter_turn(unrotated: &ImageFrame, area: Bounds) -> Vec<[u8; 4]> {
    let double_center_x = 2 * i64::from(area.x) + i64::from(area.width);
    let double_center_y = 2 * i64::from(area.y) + i64::from(area.height);
    let mut expected = Vec::new();
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx2 = 2 * i64::from(x) + 1 - double_center_x;
            let dy2 = 2 * i64::from(y) + 1 - double_center_y;
            let source_x = double_center_x + dy2 - 1;
            let source_y = double_center_y - dx2 - 1;
            assert_eq!(source_x % 2, 0, "the reference needs whole pixels");
            let pixel = u32::try_from(source_x / 2)
                .ok()
                .zip(u32::try_from(source_y / 2).ok())
                .and_then(|(sx, sy)| unrotated.pixel(sx, sy))
                .map_or([0; 4], Rgba8::to_bytes);
            expected.push(pixel);
        }
    }
    expected
}

fn pixels(frame: &ImageFrame) -> Vec<[u8; 4]> {
    (0..SIZE)
        .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
        .map(|(x, y)| frame.pixel(x, y).unwrap().to_bytes())
        .collect()
}

#[test]
fn a_quarter_turn_is_an_exact_pixel_permutation() {
    // A 8x4 box (even size difference) rotates into a 4x8 box about the same
    // centre without resampling.
    for area in [bounds(4, 6, 8, 4), bounds(4, 4, 8, 8)] {
        let (mut title, assets) = image_title(area);
        let unrotated = render(&title, ClipState::Static, &assets);
        animate(
            &mut title,
            Property::Rotation,
            Value::Scalar(0),
            Value::Scalar(180_000),
        );
        // Halfway through 0..180 degrees is exactly 90.
        let rotated = render(&title, at(HALF), &assets);
        assert_eq!(pixels(&rotated), quarter_turn(&unrotated, area), "{area:?}");
        // A whole number of turns is the identity.
        let mut repeat = title.clone();
        animate(
            &mut repeat,
            Property::Rotation,
            Value::Scalar(0),
            Value::Scalar(720_000),
        );
        assert_eq!(
            pixels(&render(&repeat, at(HALF), &assets)),
            pixels(&unrotated)
        );
    }
}

#[test]
fn an_eighth_turn_resamples_a_symmetric_diamond() {
    let mut title = template(vec![rectangle(bounds(4, 4, 8, 8))], Vec::new());
    animate(
        &mut title,
        Property::Rotation,
        Value::Scalar(0),
        Value::Scalar(90_000),
    );
    let frame = render(&title, at(HALF), &AssetCatalog::new());
    let alpha = |x: u32, y: u32| frame.pixel(x, y).unwrap().a;
    // The centre stays solid; the corners of the box and of the canvas clear.
    for (x, y) in [(7, 7), (8, 8), (7, 8), (8, 7)] {
        assert_eq!(frame.pixel(x, y).unwrap().to_bytes(), [255, 0, 0, 255]);
    }
    for (x, y) in [(4, 4), (11, 4), (4, 11), (11, 11), (0, 0), (15, 15)] {
        assert_eq!(alpha(x, y), 0, "({x}, {y})");
    }
    // The diamond's tips reach 4 * sqrt(2) ~ 5.66 px from the centre along
    // the axes: inked at 5.5, clear at 6.5.
    for (x, y) in [(13, 8), (2, 7), (8, 2), (7, 13)] {
        assert!(alpha(x, y) > 0, "({x}, {y})");
    }
    for (x, y) in [(14, 8), (1, 7), (8, 1), (7, 14)] {
        assert_eq!(alpha(x, y), 0, "({x}, {y})");
    }
    // Bilinear resampling of the hard edges keeps the area within 3%.
    let total: u32 = (0..SIZE)
        .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
        .map(|(x, y)| u32::from(alpha(x, y)))
        .sum();
    let area = 64 * 255;
    assert!(total.abs_diff(area) * 100 < area * 3, "{total} vs {area}");
    // Premultiplied output: color never exceeds alpha.
    for y in 0..SIZE {
        for x in 0..SIZE {
            let pixel = frame.pixel(x, y).unwrap();
            assert!(pixel.r <= pixel.a);
        }
    }
}

#[test]
fn an_oversized_rotated_box_is_drawn_unrotated_and_reported() {
    let side = 2_049;
    assert!(u64::from(side) * u64::from(side) > MAX_ROTATED_ELEMENT_PIXELS);
    let mut title = template(
        vec![rectangle(bounds(-1_000, -1_000, side, side))],
        Vec::new(),
    );
    title
        .edit_animation(|clip| {
            clip.set_track(Track::new(
                TrackKey::new(element_id(1), Phase::In, Property::Rotation),
                vec![Keyframe::new(0, Value::Scalar(45_000), Easing::Linear)],
            ))
        })
        .unwrap();
    let scene = title.instantiate(TitleId::new(nonzero(1))).unwrap();
    let output = ReferenceRenderer
        .render(&scene, 0, &AssetCatalog::new())
        .unwrap();
    assert_eq!(
        output.report.degraded,
        vec![DegradedElement {
            element: element_id(1),
            reason: Degradation::RotationTooLarge {
                maximum: MAX_ROTATED_ELEMENT_PIXELS
            },
        }]
    );
    assert_box(&output.frame, bounds(0, 0, SIZE, SIZE), [255, 0, 0, 255]);
}

#[test]
fn presets_and_stretch_edit_the_template_atomically() {
    let mut title = template(vec![rectangle(bounds(4, 4, 8, 8))], Vec::new());
    assert_eq!(
        title.apply_animation_preset(element_id(1), Phase::In, AnimationPreset::SlideFromLeft, 10),
        Ok(1)
    );
    assert_eq!(title.animation.clip.in_frames(), 10);
    let assets = AssetCatalog::new();
    // Frame 0 sits fully off the left edge; Hold is the static design.
    assert_box(&render(&title, at(0), &assets), bounds(0, 0, 0, 0), [0; 4]);
    assert_box(
        &render(&title, ClipState::Hold, &assets),
        bounds(4, 4, 8, 8),
        [255, 0, 0, 255],
    );
    assert_eq!(title.stretch_animation(Phase::In, 20), Ok(2));
    let key = TrackKey::new(element_id(1), Phase::In, Property::PositionX);
    let frames: Vec<u32> = title
        .animation
        .clip
        .track(key)
        .unwrap()
        .keys
        .iter()
        .map(|keyframe| keyframe.frame)
        .collect();
    assert_eq!(frames, vec![0, 20]);

    let before = title.clone();
    assert_eq!(
        title.apply_animation_preset(element_id(9), Phase::In, AnimationPreset::Fade, 5),
        Err(AnimationEditError::UnknownElement(element_id(9)))
    );
    assert!(title.stretch_animation(Phase::In, 40_000).is_err());
    assert_eq!(title, before);
}

const DIRECT_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
    "/System/Library/Fonts/Supplemental/Arial.ttf",
    "C:\\Windows\\Fonts\\arial.ttf",
];

fn font_bytes() -> Option<Vec<u8>> {
    let found = std::env::var("FM_TITLES_FONT")
        .ok()
        .and_then(|path| fs::read(path).ok())
        .or_else(|| DIRECT_FONTS.iter().find_map(|path| fs::read(path).ok()));
    if found.is_none() {
        assert!(
            std::env::var("FM_REQUIRE_FONT").as_deref() != Ok("1"),
            "FM_REQUIRE_FONT=1 but no usable font file was found"
        );
        eprintln!("skipping scaled text rendering: no font file available");
    }
    found
}

fn text_title(area: Bounds, size_px: u32) -> TitleTemplate {
    let field = FieldId::new(nonzero(1));
    let mut title = template(
        vec![Element {
            id: element_id(1),
            name: "text".into(),
            bounds: area,
            z_index: 0,
            visible: true,
            style: Style {
                font: Some(FontStyle {
                    family: "face".into(),
                    size_px,
                }),
                ..Style::default()
            },
            color_field: None,
            kind: ElementKind::Text { field },
        }],
        vec![FieldDefinition {
            id: field,
            name: "name".into(),
            default: FieldValue::Text("Scaled Ag".into()),
        }],
    );
    title.width = 400;
    title.height = 200;
    title
}

fn render_text(title: &TitleTemplate, sample: ClipState, assets: &AssetCatalog) -> Vec<u8> {
    let scene = title.instantiate(TitleId::new(nonzero(1))).unwrap();
    ReferenceRenderer
        .render_sampled(&scene, 0, assets, sample)
        .unwrap()
        .frame
        .pixels()
        .to_vec()
}

#[test]
fn scaled_text_is_laid_out_again_in_the_scaled_box_with_a_capped_font_size() {
    let Some(bytes) = font_bytes() else {
        return;
    };
    let assets = AssetCatalog::new().with_font_bytes("face", bytes).unwrap();

    // Halfway through 0..1.0: the 200x60 box becomes 100x30 about its centre
    // and the 40 px font 20 px, exactly like a static title drawn that way.
    let mut scaled = text_title(bounds(20, 20, 200, 60), 40);
    for property in [Property::ScaleX, Property::ScaleY] {
        animate(
            &mut scaled,
            property,
            Value::Scalar(0),
            Value::Scalar(10_000),
        );
    }
    let reference = text_title(bounds(70, 35, 100, 30), 20);
    let drawn = render_text(&scaled, at(HALF), &assets);
    assert!(drawn.iter().any(|channel| *channel != 0));
    assert_eq!(drawn, render_text(&reference, ClipState::Static, &assets));

    // Twice a 300 px font is capped at MAX_FONT_SIZE_PX.
    let mut capped = text_title(bounds(20, 20, 200, 60), 300);
    capped
        .edit_animation(|clip| {
            for property in [Property::ScaleX, Property::ScaleY] {
                clip.set_track(Track::new(
                    TrackKey::new(element_id(1), Phase::In, property),
                    vec![Keyframe::new(0, Value::Scalar(20_000), Easing::Linear)],
                ))?;
            }
            Ok(())
        })
        .unwrap();
    let reference = text_title(bounds(-80, -10, 400, 120), MAX_FONT_SIZE_PX);
    assert_eq!(
        render_text(&capped, ClipState::Hold, &assets),
        render_text(&reference, ClipState::Static, &assets)
    );
}
