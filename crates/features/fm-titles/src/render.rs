//! Deterministic CPU compositor for title scenes.
//!
//! [`ReferenceRenderer::render`] rasterizes a [`TitleScene`] into a
//! premultiplied RGBA8 [`ImageFrame`]. Text is real font rasterization: glyph
//! outlines come from a caller-supplied font through `ab_glyph`, with kerned
//! advances, ascent-correct baselines, word wrapping, and anti-aliased
//! coverage composited source-over into the frame. Image elements draw
//! caller-decoded premultiplied RGBA8 frames placed by a fit mode and
//! alignment and resampled with nearest or bilinear filtering, clipped to the
//! element box and the frame edge; an asset without decoded pixels keeps the
//! placeholder.
//!
//! # Determinism
//!
//! What this crate guarantees:
//!
//! * Every font metric is rounded to whole pixels (vertical) or 1/64 px
//!   (horizontal) before any pixel is touched; see [`crate::text`]. Layout,
//!   wrapping, alignment and culling are integer decisions.
//! * Glyph pen origins are always whole pixels, so each `(glyph, size)` pair is
//!   rasterized at a fixed sub-pixel phase of zero and yields the same coverage
//!   wherever it appears in a frame.
//! * Coverage is quantized once with `round(coverage * 255)`, then folded into
//!   the premultiplied blend with integer arithmetic only, in a fixed
//!   z-then-index element order.
//!
//! What this crate does **not** guarantee: the coverage values themselves.
//! Rasterization is delegated to `ab_glyph`, which selects its accumulation
//! loop at run time from the host CPU's SIMD features and tessellates curves
//! against an `f32` flatness threshold. Output is reproducible for one pinned
//! `ab_glyph` version on one CPU feature set - which is why `Cargo.toml` pins
//! an exact version rather than a caret range - but is not promised to be
//! byte-identical across machines. Do not compare frame hashes across hosts.
//!
//! # Clipping
//!
//! Text that does not fit is **clipped**, never truncated with an ellipsis and
//! never spilled: wrapping keeps lines inside the element width, and every
//! glyph pixel is tested against the element box and then against the frame.
//!
//! # Bounds
//!
//! Output dimensions, font size, characters per element, glyphs per frame, and
//! the per-glyph rasterization area are all capped, and every fill iterates the
//! element's intersection with the canvas rather than its declared extent, so
//! no operator-supplied number ever sets a loop length. A cap that is reached
//! while drawing one element degrades that element and is listed in
//! [`RenderReport::degraded`]; the frame still renders. Only a cap that makes
//! the whole frame meaningless is a [`RenderError`].

use crate::{
    Alignment, AssetCatalog, Bounds, Color, Crawl, CrawlMode, Element, ElementId, ElementKind,
    FieldId, FieldValue, FontFace, HorizontalAlignment, ImageFilter, ImageFit, MissingFont,
    TickerSpec, TitleScene, VerticalAlignment,
    animation::evaluate_tracks,
    crawl::{MAX_CRAWL_VISIBLE_SEGMENTS, layout_segment},
    evaluate_clock, evaluate_ticker_position,
    font::ScaledFace,
    image,
    text::{LaidText, fixed_to_px, layout_text},
    validate_scene,
};
use ab_glyph::{Font, Glyph, GlyphId, OutlinedGlyph, ScaleFont, point};
use core::fmt;
use fm_video::{FrameError, ImageFrame, Rgba8};

/// Maximum rendered frame width.
pub const MAX_OUTPUT_WIDTH: u32 = 8_192;
/// Maximum rendered frame height.
pub const MAX_OUTPUT_HEIGHT: u32 = 8_192;
/// Maximum font pixel size (ascent to descent) accepted for an element.
pub const MAX_FONT_SIZE_PX: u32 = 512;
/// Maximum characters laid out for a single element. Newlines, tabs and spaces
/// each cost one, so this bounds layout time and allocation for any input.
pub const MAX_GLYPHS_PER_ELEMENT: usize = 4_096;
/// Maximum glyphs rasterized for a whole frame.
pub const MAX_GLYPHS_PER_FRAME: usize = 65_536;
/// Maximum width or height of one glyph's rasterization box, in pixels.
pub const MAX_GLYPH_RASTER_DIMENSION: u32 = 4_096;
/// Maximum area of one glyph's rasterization box, in pixels. Bounds the
/// transient coverage buffer `ab_glyph` allocates while rasterizing.
pub const MAX_GLYPH_RASTER_PIXELS: u64 = 4 * 1024 * 1024;

/// Glyphs whose pen origin lies further than `MAX_FONT_SIZE_PX * this` outside
/// the element box cannot contribute ink and are skipped before rasterizing.
const CULL_MARGIN_FACTOR: i64 = 16;

pub const REFERENCE_RENDERER_LIMITATIONS: &[&str] = &[
    "text is laid out in codepoint order: no bidi, no complex-script shaping, no ligatures or other OpenType features",
    "image elements place decoded frames with integer stretch/contain/cover/none fits and nearest or 8-bit fixed-point bilinear sampling: no mipmapping or higher-order filters for strong minification, no color profile handling, orientation, or animated image integration",
    "rasterization is CPU-only and layout/animation use integer arithmetic without production GPU effects",
];

/// Why one element was drawn incompletely.
///
/// A degradation is never silent and never fatal: the element is drawn as far
/// as its cap allowed, the rest of the frame renders normally, and the reason
/// is reported so an operator or log can see which field was too long.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Degradation {
    /// The element's text exceeded [`MAX_GLYPHS_PER_ELEMENT`] characters. The
    /// leading characters were laid out and drawn; the rest were dropped.
    TextTruncated { maximum: usize },
    /// The frame's [`MAX_GLYPHS_PER_FRAME`] budget ran out. The element's
    /// remaining glyphs were not rasterized.
    GlyphBudgetExhausted { maximum: usize },
    /// A crawl had more than this many segments inside its viewport; the
    /// trailing ones were not drawn.
    CrawlSegmentsCapped { maximum: usize },
}

/// One element that could not be drawn in full, and why.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DegradedElement {
    pub element: ElementId,
    pub reason: Degradation,
}

impl fmt::Display for DegradedElement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.reason {
            Degradation::TextTruncated { maximum } => write!(
                formatter,
                "element {} text was truncated at {maximum} characters",
                self.element
            ),
            Degradation::GlyphBudgetExhausted { maximum } => write!(
                formatter,
                "element {} lost glyphs to the {maximum} glyph frame budget",
                self.element
            ),
            Degradation::CrawlSegmentsCapped { maximum } => write!(
                formatter,
                "crawl {} drew only its first {maximum} visible segments",
                self.element
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderReport {
    pub missing_fonts: Vec<crate::MissingFont>,
    pub missing_images: Vec<crate::MissingImage>,
    /// Elements drawn incompletely because they reached a cap. Empty on a
    /// fully rendered frame.
    pub degraded: Vec<DegradedElement>,
    pub limitations: &'static [&'static str],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderOutput {
    pub frame: ImageFrame,
    pub report: RenderReport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderError {
    Frame(FrameError),
    /// The scene canvas exceeds [`MAX_OUTPUT_WIDTH`] or [`MAX_OUTPUT_HEIGHT`].
    OutputTooLarge {
        width: u32,
        height: u32,
        max_width: u32,
        max_height: u32,
    },
    /// The element's font size exceeds [`MAX_FONT_SIZE_PX`].
    FontSizeTooLarge {
        element: ElementId,
        size_px: u32,
        maximum: u32,
    },
    /// A single glyph would rasterize into an oversized coverage buffer.
    GlyphTooLarge {
        element: ElementId,
        width: u32,
        height: u32,
    },
    /// A crawl was asked for a frame before its last accepted item update.
    CrawlFrameRegressed {
        element: ElementId,
        frame: u64,
        last_frame: u64,
    },
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => error.fmt(formatter),
            Self::OutputTooLarge {
                width,
                height,
                max_width,
                max_height,
            } => write!(
                formatter,
                "output {width}x{height} exceeds maximum {max_width}x{max_height}"
            ),
            Self::FontSizeTooLarge {
                element,
                size_px,
                maximum,
            } => write!(
                formatter,
                "element {element} font size {size_px}px exceeds maximum {maximum}px"
            ),
            Self::GlyphTooLarge {
                element,
                width,
                height,
            } => write!(
                formatter,
                "element {element} has a glyph rasterizing to {width}x{height} pixels"
            ),
            Self::CrawlFrameRegressed {
                element,
                frame,
                last_frame,
            } => write!(
                formatter,
                "crawl {element} frame {frame} precedes its last update frame {last_frame}"
            ),
        }
    }
}

impl std::error::Error for RenderError {}

impl From<FrameError> for RenderError {
    fn from(value: FrameError) -> Self {
        Self::Frame(value)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ReferenceRenderer;

/// A live [`Crawl`] drawn over a scene at one engine frame index.
#[derive(Clone, Copy, Debug)]
pub struct CrawlLayer<'a> {
    pub crawl: &'a Crawl,
    pub frame: u64,
}

impl ReferenceRenderer {
    /// Renders a premultiplied RGBA8 frame in deterministic z/index order.
    ///
    /// Elements whose font family is absent from `assets` draw their
    /// background but no text, and are listed in
    /// [`RenderReport::missing_fonts`]; a missing asset degrades one element
    /// instead of failing the frame. An image element draws the decoded frame
    /// registered for its field's asset, fitted and filtered inside the element
    /// bounds as the element's [`ElementKind::Image`] requests; an
    /// asset with no decoded pixels keeps the placeholder. An element that
    /// reaches the per-element character cap or exhausts the frame glyph budget
    /// is likewise drawn as far as it can be and listed in
    /// [`RenderReport::degraded`]: one over-long field must not take the whole
    /// frame off air.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] when the canvas, a font size, or a single
    /// glyph's rasterization box exceeds its cap, or when the output frame
    /// layout is invalid.
    pub fn render(
        self,
        scene: &TitleScene,
        scene_time_ms: u64,
        assets: &AssetCatalog,
    ) -> Result<RenderOutput, RenderError> {
        self.render_with_crawls(scene, scene_time_ms, assets, &[])
    }

    /// Renders like [`Self::render`], then draws each crawl layer on top of
    /// every scene element, in slice order.
    ///
    /// Each crawl paints its optional background over its viewport and then
    /// the items and separators [`Crawl::view`] reports for the layer's frame,
    /// clipped to the viewport: a crawl item's pen starts at
    /// `bounds.x + segment.offset` and is vertically aligned like a text
    /// element; a roll item's line box starts at `bounds.y + segment.offset`
    /// and each wrapped line is horizontally aligned. A crawl whose font is
    /// absent is listed in [`RenderReport::missing_fonts`] and draws only its
    /// background.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::render`], or
    /// [`RenderError::CrawlFrameRegressed`] when a layer's frame precedes its
    /// crawl's last accepted update.
    pub fn render_with_crawls(
        self,
        scene: &TitleScene,
        scene_time_ms: u64,
        assets: &AssetCatalog,
        crawls: &[CrawlLayer<'_>],
    ) -> Result<RenderOutput, RenderError> {
        if scene.width() > MAX_OUTPUT_WIDTH || scene.height() > MAX_OUTPUT_HEIGHT {
            return Err(RenderError::OutputTooLarge {
                width: scene.width(),
                height: scene.height(),
                max_width: MAX_OUTPUT_WIDTH,
                max_height: MAX_OUTPUT_HEIGHT,
            });
        }
        let stride = usize::try_from(scene.width())
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or(FrameError::LayoutOverflow)?;
        let length = stride
            .checked_mul(usize::try_from(scene.height()).map_err(|_| FrameError::LayoutOverflow)?)
            .ok_or(FrameError::LayoutOverflow)?;
        if length > ImageFrame::MAX_BUFFER_BYTES {
            return Err(FrameError::BufferTooLarge {
                required: length,
                maximum: ImageFrame::MAX_BUFFER_BYTES,
            }
            .into());
        }
        let mut pixels = vec![0; length];
        let mut canvas = Canvas {
            pixels: &mut pixels,
            width: scene.width(),
            height: scene.height(),
        };
        canvas.fill(scene.background());

        let mut elements: Vec<(usize, &Element)> = scene.elements().iter().enumerate().collect();
        elements.sort_by_key(|(index, element)| (element.z_index, *index));
        let mut state = FrameState {
            glyphs: MAX_GLYPHS_PER_FRAME,
            degraded: Vec::new(),
        };
        for (_, element) in elements {
            if element.visible {
                draw_element(
                    &mut canvas,
                    scene,
                    element,
                    scene_time_ms,
                    assets,
                    &mut state,
                )?;
            }
        }

        let mut validation = validate_scene(scene, assets);
        for layer in crawls {
            draw_crawl(
                &mut canvas,
                *layer,
                assets,
                &mut state,
                &mut validation.missing_fonts,
            )?;
        }
        Ok(RenderOutput {
            frame: ImageFrame::new(scene.width(), scene.height(), stride, pixels)?,
            report: RenderReport {
                missing_fonts: validation.missing_fonts,
                missing_images: validation.missing_images,
                degraded: state.degraded,
                limitations: REFERENCE_RENDERER_LIMITATIONS,
            },
        })
    }
}

/// The frame's remaining glyph budget and the degradations spending it caused.
struct FrameState {
    glyphs: usize,
    degraded: Vec<DegradedElement>,
}

impl FrameState {
    /// Spends one glyph from the frame budget, or reports it is gone.
    fn take_glyph(&mut self) -> bool {
        if self.glyphs == 0 {
            return false;
        }
        self.glyphs -= 1;
        true
    }

    /// Records one reason per element: a cap reached on every glyph of a long
    /// line must not grow the report line by line.
    fn degrade(&mut self, element: ElementId, reason: Degradation) {
        let entry = DegradedElement { element, reason };
        if !self.degraded.contains(&entry) {
            self.degraded.push(entry);
        }
    }
}

fn draw_element(
    canvas: &mut Canvas<'_>,
    scene: &TitleScene,
    element: &Element,
    time_ms: u64,
    assets: &AssetCatalog,
    state: &mut FrameState,
) -> Result<(), RenderError> {
    let evaluated = evaluate_tracks(
        element.bounds,
        element.style.opacity,
        &element.animations,
        time_ms,
    );
    if evaluated.bounds.width == 0 || evaluated.bounds.height == 0 {
        return Ok(());
    }
    let fill = element
        .color_field
        .and_then(|field| scene.field_value(field))
        .and_then(|value| match value {
            FieldValue::Color(color) => Some(*color),
            _ => None,
        })
        .unwrap_or(element.style.fill);

    match element.kind {
        ElementKind::Rectangle => {
            canvas.rect(evaluated.bounds, fill, evaluated.opacity);
            return Ok(());
        }
        ElementKind::Image { field, fit, filter } => {
            ImagePaint {
                element,
                bounds: evaluated.bounds,
                field,
                fit,
                filter,
                fill,
                opacity: evaluated.opacity,
            }
            .paint(canvas, scene, assets);
            return Ok(());
        }
        ElementKind::Text { .. } | ElementKind::Clock(_) | ElementKind::Ticker(_) => {}
    }

    if let Some(background) = element.style.background {
        canvas.rect(evaluated.bounds, background, evaluated.opacity);
    }
    let Some(style) = element.style.font.as_ref() else {
        return Ok(());
    };
    if style.size_px > MAX_FONT_SIZE_PX {
        return Err(RenderError::FontSizeTooLarge {
            element: element.id,
            size_px: style.size_px,
            maximum: MAX_FONT_SIZE_PX,
        });
    }
    // A family the caller never supplied bytes for is reported as a missing
    // asset by the render report; the element keeps its background and draws
    // no text rather than substituting another face.
    let Some(face) = assets.font(&style.family) else {
        return Ok(());
    };
    // Nothing of this element is on the canvas, so no glyph of it can be.
    let Some(region) = canvas.region(evaluated.bounds) else {
        return Ok(());
    };

    let paint = TextPaint {
        element: element.id,
        bounds: evaluated.bounds,
        region,
        color: fill,
        opacity: evaluated.opacity,
        alignment: element.style.alignment,
        face,
        size_px: style.size_px.max(1),
    };
    match element.kind {
        ElementKind::Text { field } => {
            let Some(text) = scene.field_value(field).and_then(FieldValue::display_text) else {
                return Ok(());
            };
            paint.paint(canvas, &text, TextFlow::Wrapped, state)
        }
        ElementKind::Clock(spec) => paint.paint(
            canvas,
            &evaluate_clock(spec, time_ms),
            TextFlow::Wrapped,
            state,
        ),
        ElementKind::Ticker(spec) => {
            let Some(text) = scene
                .field_value(spec.field)
                .and_then(FieldValue::display_text)
            else {
                return Ok(());
            };
            paint.paint(canvas, &text, TextFlow::Ticker { spec, time_ms }, state)
        }
        ElementKind::Rectangle | ElementKind::Image { .. } => Ok(()),
    }
}

/// What an image element draws and where.
///
/// Decoded pixels replace the placeholder wholesale: a decoded frame is drawn
/// fitted inside the element bounds, while the placeholder stays for a
/// name-only asset, an unknown asset (reported as missing by `validate_scene`),
/// or a field that is absent or of another type. `style.background` is part of
/// the placeholder drawing and is not painted under decoded pixels.
struct ImagePaint<'a> {
    element: &'a Element,
    bounds: Bounds,
    field: FieldId,
    fit: ImageFit,
    filter: ImageFilter,
    fill: Color,
    opacity: u8,
}

impl ImagePaint<'_> {
    fn paint(&self, canvas: &mut Canvas<'_>, scene: &TitleScene, assets: &AssetCatalog) {
        let decoded = scene
            .field_value(self.field)
            .and_then(|value| match value {
                FieldValue::Image(image) => Some(image.asset.as_str()),
                _ => None,
            })
            .and_then(|asset| assets.image(asset));
        if let Some(frame) = decoded {
            canvas.image(
                self.bounds,
                frame,
                ImageLayout {
                    fit: self.fit,
                    filter: self.filter,
                    alignment: self.element.style.alignment,
                },
                self.opacity,
            );
        } else {
            draw_placeholder(
                canvas,
                self.bounds,
                self.element
                    .style
                    .background
                    .unwrap_or(Color::new(64, 64, 64, 255)),
                self.fill,
                self.opacity,
            );
        }
    }
}

/// How a text element consumes its box horizontally.
#[derive(Clone, Copy, Debug)]
enum TextFlow {
    /// Word-wrapped inside the element width and aligned by style.
    Wrapped,
    /// One unwrapped line scrolled by the ticker's evaluated position.
    Ticker { spec: TickerSpec, time_ms: u64 },
}

struct TextPaint<'a> {
    element: ElementId,
    bounds: Bounds,
    /// `bounds` intersected with the canvas: the only pixels this element can
    /// ever ink, and the reference for every cull below.
    region: Region,
    color: Color,
    opacity: u8,
    alignment: Alignment,
    face: &'a FontFace,
    size_px: u32,
}

impl TextPaint<'_> {
    fn paint(
        &self,
        canvas: &mut Canvas<'_>,
        text: &str,
        flow: TextFlow,
        state: &mut FrameState,
    ) -> Result<(), RenderError> {
        let scaled = self.face.scaled(self.size_px);
        let wrap = match flow {
            TextFlow::Wrapped => Some(self.bounds.width),
            TextFlow::Ticker { .. } => None,
        };
        let laid = layout_text(&scaled, text, wrap, MAX_GLYPHS_PER_ELEMENT);
        if laid.truncated {
            // Draw the part that fit and tell the caller which element lost
            // text: an over-long field costs its own tail, not the frame.
            state.degrade(
                self.element,
                Degradation::TextTruncated {
                    maximum: MAX_GLYPHS_PER_ELEMENT,
                },
            );
        }
        let forced_x = match flow {
            TextFlow::Wrapped => None,
            TextFlow::Ticker { spec, time_ms } => Some(evaluate_ticker_position(
                spec,
                time_ms,
                self.bounds.width,
                laid.width_px,
            )),
        };
        self.paint_lines(canvas, &scaled, &laid, (forced_x, None), state)
    }

    fn paint_lines(
        &self,
        canvas: &mut Canvas<'_>,
        scaled: &ScaledFace<'_>,
        laid: &LaidText,
        forced: (Option<i64>, Option<i64>),
        state: &mut FrameState,
    ) -> Result<(), RenderError> {
        let (forced_x, forced_top) = forced;
        let count = i64::try_from(laid.lines.len()).unwrap_or(i64::MAX);
        let block_height = laid.line_height_px.saturating_mul(count);
        let top = i64::from(self.bounds.y).saturating_add(forced_top.unwrap_or_else(|| {
            aligned_offset(self.bounds.height, block_height, self.alignment.vertical)
        }));
        let margin = self.cull_margin();
        for (index, line) in laid.lines.iter().enumerate() {
            let offset = laid
                .line_height_px
                .saturating_mul(i64::try_from(index).unwrap_or(i64::MAX));
            let baseline = top
                .saturating_add(laid.ascent_px)
                .saturating_add(offset)
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX));
            if baseline < i64::from(self.region.y0).saturating_sub(margin)
                || baseline > i64::from(self.region.y1).saturating_add(margin)
            {
                continue;
            }
            let width = u32::try_from(fixed_to_px(line.width_fixed).max(0)).unwrap_or(u32::MAX);
            let aligned = forced_x.unwrap_or_else(|| {
                aligned_offset_horizontal(self.bounds.width, width, self.alignment)
            });
            let origin = i64::from(self.bounds.x).saturating_add(aligned);
            for glyph in &line.glyphs {
                let pen = origin
                    .saturating_add(fixed_to_px(glyph.x_fixed))
                    .clamp(i64::from(i32::MIN), i64::from(i32::MAX));
                if !self.draw_glyph(canvas, scaled, glyph.id, (pen, baseline), state)? {
                    state.degrade(
                        self.element,
                        Degradation::GlyphBudgetExhausted {
                            maximum: MAX_GLYPHS_PER_FRAME,
                        },
                    );
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// How far outside the visible region a pen origin may sit and still ink a
    /// visible pixel. At most `MAX_FONT_SIZE_PX * CULL_MARGIN_FACTOR` = 8192.
    fn cull_margin(&self) -> i64 {
        i64::from(self.size_px).saturating_mul(CULL_MARGIN_FACTOR)
    }

    /// Draws one glyph. `Ok(false)` means the frame glyph budget is exhausted
    /// and the caller must stop drawing this element.
    fn draw_glyph(
        &self,
        canvas: &mut Canvas<'_>,
        scaled: &ScaledFace<'_>,
        id: GlyphId,
        origin: (i64, i64),
        state: &mut FrameState,
    ) -> Result<bool, RenderError> {
        let (pen_x, baseline_y) = origin;
        let margin = self.cull_margin();
        let region = self.region;
        let outside = pen_x < i64::from(region.x0).saturating_sub(margin)
            || pen_x > i64::from(region.x1).saturating_add(margin)
            || baseline_y < i64::from(region.y0).saturating_sub(margin)
            || baseline_y > i64::from(region.y1).saturating_add(margin);
        if outside {
            return Ok(true);
        }
        // The cull above is relative to the visible region, whose coordinates
        // are canvas coordinates and so at most MAX_OUTPUT_WIDTH/HEIGHT (8192),
        // widened by at most 8192. A surviving position is therefore within
        // +-16384 and always converts; the fallible conversion stays as a guard
        // rather than as the bound. `f32::from(i16)` is exact, so the glyph
        // rasterizes at sub-pixel phase 0.
        let (Ok(pen), Ok(baseline)) = (i16::try_from(pen_x), i16::try_from(baseline_y)) else {
            return Ok(true);
        };
        let Some(outline) = scaled.font().outline(id) else {
            return Ok(true);
        };

        let scale_factor = scaled.scale_factor();
        let position = point(f32::from(pen), f32::from(baseline));
        let px_bounds = outline.px_bounds(scale_factor, position);
        let width = raster_dimension(px_bounds.width());
        let height = raster_dimension(px_bounds.height());
        if width == 0 || height == 0 {
            return Ok(true);
        }
        let area = u64::from(width) * u64::from(height);
        if width > MAX_GLYPH_RASTER_DIMENSION
            || height > MAX_GLYPH_RASTER_DIMENSION
            || area > MAX_GLYPH_RASTER_PIXELS
        {
            return Err(RenderError::GlyphTooLarge {
                element: self.element,
                width,
                height,
            });
        }

        let base_x = round_to_i64(px_bounds.min.x);
        let base_y = round_to_i64(px_bounds.min.y);
        // A raster box that cannot touch the visible region would have every
        // one of its up to MAX_GLYPH_RASTER_PIXELS coverage samples discarded.
        // Skip it instead of rasterizing it.
        if !region.intersects(base_x, base_y, width, height) {
            return Ok(true);
        }
        if !state.take_glyph() {
            return Ok(false);
        }

        let glyph = Glyph {
            id,
            scale: scaled.scale(),
            position,
        };
        let (color, opacity) = (self.color, self.opacity);
        OutlinedGlyph::new(glyph, outline, scale_factor).draw(|x, y, coverage| {
            let x = base_x.saturating_add(i64::from(x));
            let y = base_y.saturating_add(i64::from(y));
            if region.contains(x, y) {
                canvas.blend(x, y, color, multiply_u8(opacity, coverage_to_u8(coverage)));
            }
        });
        Ok(true)
    }
}

/// Draws one crawl layer; see [`ReferenceRenderer::render_with_crawls`].
fn draw_crawl(
    canvas: &mut Canvas<'_>,
    layer: CrawlLayer<'_>,
    assets: &AssetCatalog,
    state: &mut FrameState,
    missing_fonts: &mut Vec<MissingFont>,
) -> Result<(), RenderError> {
    let crawl = layer.crawl;
    let config = crawl.config();
    let view = crawl
        .view(layer.frame)
        .map_err(|_| RenderError::CrawlFrameRegressed {
            element: crawl.id(),
            frame: layer.frame,
            last_frame: crawl.last_frame(),
        })?;
    if let Some(background) = config.background {
        canvas.rect(config.bounds, background, config.opacity);
    }
    // `Crawl::new` refuses oversized fonts, so this only guards the cast.
    let size_px = config.font.size_px.clamp(1, MAX_FONT_SIZE_PX);
    let Some(face) = assets.font(&config.font.family) else {
        missing_fonts.push(MissingFont {
            element: crawl.id(),
            family: config.font.family.clone(),
        });
        return Ok(());
    };
    let Some(region) = canvas.region(config.bounds) else {
        return Ok(());
    };
    let paint = TextPaint {
        element: crawl.id(),
        bounds: config.bounds,
        region,
        color: config.fill,
        opacity: config.opacity,
        alignment: config.alignment,
        face,
        size_px,
    };
    let scaled = face.scaled(size_px);
    for segment in &view.segments {
        if state.glyphs == 0 {
            state.degrade(
                crawl.id(),
                Degradation::GlyphBudgetExhausted {
                    maximum: MAX_GLYPHS_PER_FRAME,
                },
            );
            break;
        }
        let laid = layout_segment(&scaled, config.mode, config.bounds.width, segment.text);
        let forced = match config.mode {
            CrawlMode::Crawl => (Some(segment.offset), None),
            CrawlMode::Roll => (None, Some(segment.offset)),
        };
        paint.paint_lines(canvas, &scaled, &laid, forced, state)?;
    }
    if view.truncated {
        state.degrade(
            crawl.id(),
            Degradation::CrawlSegmentsCapped {
                maximum: MAX_CRAWL_VISIBLE_SEGMENTS,
            },
        );
    }
    Ok(())
}

/// How a decoded image is fitted, aligned, and filtered inside its bounds.
#[derive(Clone, Copy, Debug)]
struct ImageLayout {
    fit: ImageFit,
    filter: ImageFilter,
    alignment: Alignment,
}

/// A half-open rectangle of canvas pixels, `x0..x1` by `y0..y1`, already
/// intersected with the canvas: every coordinate in it indexes a real pixel, so
/// iterating it can never run longer than the canvas.
#[derive(Clone, Copy, Debug)]
struct Region {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Region {
    fn contains(self, x: i64, y: i64) -> bool {
        x >= i64::from(self.x0)
            && x < i64::from(self.x1)
            && y >= i64::from(self.y0)
            && y < i64::from(self.y1)
    }

    /// Whether a `width` by `height` box with its top-left corner at `(x, y)`
    /// overlaps this region.
    fn intersects(self, x: i64, y: i64, width: u32, height: u32) -> bool {
        x < i64::from(self.x1)
            && y < i64::from(self.y1)
            && x.saturating_add(i64::from(width)) > i64::from(self.x0)
            && y.saturating_add(i64::from(height)) > i64::from(self.y0)
    }
}

struct Canvas<'a> {
    pixels: &'a mut [u8],
    width: u32,
    height: u32,
}

impl Canvas<'_> {
    fn fill(&mut self, color: Color) {
        let color = premultiply(color, u8::MAX);
        for pixel in self.pixels.as_chunks_mut::<4>().0 {
            *pixel = color;
        }
    }

    /// Intersects an element rectangle with the canvas, in `i64` so no extent
    /// can wrap. `None` means the element is entirely off-canvas and must not
    /// be iterated at all.
    ///
    /// Every fill goes through here. Element extents are `u32` and animatable
    /// up to `u32::MAX`, so iterating a declared extent and discarding the
    /// off-canvas pixels one at a time would make the loop length
    /// operator-controlled instead of canvas-controlled.
    fn region(&self, bounds: Bounds) -> Option<Region> {
        let width = i64::from(self.width);
        let height = i64::from(self.height);
        let left = i64::from(bounds.x);
        let top = i64::from(bounds.y);
        let x0 = left.clamp(0, width);
        let y0 = top.clamp(0, height);
        let x1 = left.saturating_add(i64::from(bounds.width)).clamp(0, width);
        let y1 = top
            .saturating_add(i64::from(bounds.height))
            .clamp(0, height);
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        Some(Region {
            x0: u32::try_from(x0).ok()?,
            y0: u32::try_from(y0).ok()?,
            x1: u32::try_from(x1).ok()?,
            y1: u32::try_from(y1).ok()?,
        })
    }

    fn rect(&mut self, bounds: Bounds, color: Color, opacity: u8) {
        let Some(region) = self.region(bounds) else {
            return;
        };
        for y in region.y0..region.y1 {
            for x in region.x0..region.x1 {
                self.blend_pixel(x, y, color, opacity);
            }
        }
    }

    /// Draws a decoded premultiplied RGBA8 frame placed inside `bounds` by
    /// `layout`, clipped to both `bounds` and this canvas.
    ///
    /// [`crate::image::place`] computes the placed rectangle from the fit mode
    /// and alignment; each destination pixel inside it samples the source with
    /// the layout's filter (see [`crate::ImageFilter`]). With
    /// [`ImageFit::Stretch`] and [`ImageFilter::Nearest`] destination offset
    /// `d` samples `floor(d * source / bound)` on each axis, matching
    /// `fm_video::transform_nearest`. The sample is scaled by `opacity` and
    /// composited source-over by [`Self::blend_premultiplied`].
    ///
    /// Only the intersection of the canvas, `bounds`, and the placed rectangle
    /// is iterated, and each destination pixel reads at most four source
    /// pixels: work is bounded by the canvas, never by the decoded frame's
    /// size, the placed size, or the element's declared extent.
    fn image(&mut self, bounds: Bounds, frame: &ImageFrame, layout: ImageLayout, opacity: u8) {
        let Some(region) = self.region(bounds) else {
            return;
        };
        let Some(placement) = image::place(
            bounds,
            frame.width(),
            frame.height(),
            layout.fit,
            layout.alignment,
        ) else {
            return;
        };
        // Intersect one axis of the clipped element region with the placed
        // image; `None` when they do not overlap.
        let clip = |start: u32, end: u32, origin: i128, extent: i128| -> Option<(u32, u32)> {
            let low = i128::from(start).max(origin);
            let high = i128::from(end).min(origin.checked_add(extent)?);
            if low >= high {
                return None;
            }
            Some((u32::try_from(low).ok()?, u32::try_from(high).ok()?))
        };
        let Some((x0, x1)) = clip(region.x0, region.x1, placement.x, placement.width) else {
            return;
        };
        let Some((y0, y1)) = clip(region.y0, region.y1, placement.y, placement.height) else {
            return;
        };
        for y in y0..y1 {
            let local_y = i128::from(y) - placement.y;
            for x in x0..x1 {
                let local_x = i128::from(x) - placement.x;
                if let Some(pixel) =
                    image::sample(frame, placement, local_x, local_y, layout.filter)
                {
                    self.blend_premultiplied(x, y, pixel, opacity);
                }
            }
        }
    }

    /// Source-over composite of one already-premultiplied RGBA8 source pixel
    /// scaled by `opacity`.
    ///
    /// Every channel, alpha included, is scaled by `opacity` with
    /// [`multiply_u8`] - for a premultiplied source this is exactly scaling the
    /// pixel's coverage, whereas re-multiplying by the derived alpha would
    /// apply alpha twice. The scaled source is then blended per channel as
    /// `destination = source_scaled + round(destination * (255 - source_alpha) / 255)`
    /// with saturating addition.
    fn blend_premultiplied(&mut self, x: u32, y: u32, pixel: Rgba8, opacity: u8) {
        let Some(offset) = pixel_offset(self.width, x, y) else {
            return;
        };
        let Some(destination) = offset
            .checked_add(4)
            .and_then(|end| self.pixels.get_mut(offset..end))
        else {
            return;
        };
        let alpha = multiply_u8(pixel.a, opacity);
        let source = [
            multiply_u8(pixel.r, opacity),
            multiply_u8(pixel.g, opacity),
            multiply_u8(pixel.b, opacity),
            alpha,
        ];
        let inverse = u8::MAX - alpha;
        for (channel, value) in destination.iter_mut().zip(source) {
            *channel = value.saturating_add(multiply_u8(*channel, inverse));
        }
    }

    /// Source-over blend at an arbitrary coordinate, discarding anything off
    /// canvas. Only for coordinates this crate does not choose itself, such as
    /// the ones `ab_glyph` reports while rasterizing a glyph.
    fn blend(&mut self, x: i64, y: i64, color: Color, alpha: u8) {
        let Ok(x) = u32::try_from(x) else { return };
        let Ok(y) = u32::try_from(y) else { return };
        if x >= self.width || y >= self.height {
            return;
        }
        self.blend_pixel(x, y, color, alpha);
    }

    /// Source-over blend of `color` scaled by `alpha` into premultiplied RGBA8.
    fn blend_pixel(&mut self, x: u32, y: u32, color: Color, alpha: u8) {
        let Some(offset) = pixel_offset(self.width, x, y) else {
            return;
        };
        let Some(destination) = offset
            .checked_add(4)
            .and_then(|end| self.pixels.get_mut(offset..end))
        else {
            return;
        };
        let source = premultiply(color, alpha);
        let inverse = u8::MAX - source[3];
        // Premultiplied source-over, identical for colour and alpha channels.
        for (channel, value) in destination.iter_mut().zip(source) {
            *channel = value.saturating_add(multiply_u8(*channel, inverse));
        }
    }
}

fn draw_placeholder(
    canvas: &mut Canvas<'_>,
    bounds: Bounds,
    background: Color,
    mark: Color,
    opacity: u8,
) {
    canvas.rect(bounds, background, opacity);
    let Some(region) = canvas.region(bounds) else {
        return;
    };
    let last_x = bounds.width.saturating_sub(1);
    let last_y = bounds.height.saturating_sub(1);
    // Rows come from the canvas-clipped region; the cross positions stay
    // element-relative, so the mark is identical to an unclipped draw.
    for y in region.y0..region.y1 {
        let local_y = u32::try_from(i64::from(y).saturating_sub(i64::from(bounds.y))).unwrap_or(0);
        let diagonal = if bounds.height <= 1 {
            0
        } else {
            u64::from(local_y) * u64::from(last_x) / u64::from(last_y.max(1))
        };
        let diagonal = u32::try_from(diagonal).unwrap_or(last_x);
        for local_x in [diagonal, last_x.saturating_sub(diagonal)] {
            canvas.blend(
                i64::from(bounds.x) + i64::from(local_x),
                i64::from(y),
                mark,
                opacity,
            );
        }
    }
}

fn aligned_offset_horizontal(viewport: u32, content: u32, alignment: Alignment) -> i64 {
    match alignment.horizontal {
        HorizontalAlignment::Start => 0,
        HorizontalAlignment::Center => (i64::from(viewport) - i64::from(content)) / 2,
        HorizontalAlignment::End => i64::from(viewport) - i64::from(content),
    }
}

fn aligned_offset(viewport: u32, content: i64, alignment: VerticalAlignment) -> i64 {
    match alignment {
        VerticalAlignment::Start => 0,
        VerticalAlignment::Center => (i64::from(viewport).saturating_sub(content)) / 2,
        VerticalAlignment::End => i64::from(viewport).saturating_sub(content),
    }
}

fn pixel_offset(width: u32, x: u32, y: u32) -> Option<usize> {
    usize::try_from(y)
        .ok()?
        .checked_mul(usize::try_from(width).ok()?)?
        .checked_add(usize::try_from(x).ok()?)?
        .checked_mul(4)
}

fn premultiply(color: Color, opacity: u8) -> [u8; 4] {
    let alpha = multiply_u8(color.a, opacity);
    [
        multiply_u8(color.r, alpha),
        multiply_u8(color.g, alpha),
        multiply_u8(color.b, alpha),
        alpha,
    ]
}

fn multiply_u8(left: u8, right: u8) -> u8 {
    let product = u16::from(left) * u16::from(right) + 127;
    u8::try_from(product / 255).unwrap_or(u8::MAX)
}

/// Quantizes anti-aliased coverage to 8 bits with round-half-up. Values outside
/// `0..=1` and non-finite values are clamped, so a degenerate outline cannot
/// produce out-of-range alpha.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn coverage_to_u8(coverage: f32) -> u8 {
    if !coverage.is_finite() || coverage <= 0.0 {
        return 0;
    }
    if coverage >= 1.0 {
        return u8::MAX;
    }
    (coverage * 255.0 + 0.5) as u8
}

/// Converts a glyph rasterization extent to whole pixels. `f32 as u32`
/// saturates in Rust; the guards keep NaN and negatives at zero.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn raster_dimension(value: f32) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    value as u32
}

/// Rounds an integral `f32` pixel coordinate. `f32 as i64` saturates.
#[allow(clippy::cast_possible_truncation)]
fn round_to_i64(value: f32) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    value.round() as i64
}
