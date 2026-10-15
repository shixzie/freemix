//! Deterministic CPU reference for geometric two-source transitions.
//!
//! Every shape mixes a premultiplied RGBA8 Program (`from`) toward a Preview
//! (`to`) at the exact rational progress `p = numerator / denominator`. Zero
//! progress returns `from` and full progress returns `to`, byte for byte.
//!
//! Integer shapes (wipes, slides, Zoom, Merge) copy whole texels at floored
//! integer extents ([`transition_extent`]). Geometric shapes inverse-map each
//! output pixel centre to a source texel coordinate with IEEE `f64` basic
//! arithmetic only (no transcendental functions: angles come from the shared
//! Q30 table [`fm_animation::sin_cos_q30`], exact at multiples of 90 degrees),
//! floor that coordinate once to 1/256 texel, and sample the premultiplied
//! source bilinearly with 8-bit weights, rounding
//! `(top * (256 - fy) + bottom * fy + 32768) >> 16`, transparent outside the
//! source: the arbitrary-angle layer rotation's sampler. Texel coordinates are
//! texel-centre based (texel `x` covers `[x - 0.5, x + 0.5)`), and screen
//! coordinates are relative to the frame centre: `sx = x + 0.5 - W / 2`,
//! `sy = y + 0.5 - H / 2`, `y` down. "Over" is premultiplied source-over,
//! `src + (dst * (255 - src.a) + 127) / 255` per channel (saturating).

use fm_animation::{TRIG_ONE, sin_cos_q30};

use crate::blend::{blend_channel, validate_inputs};
use crate::composite::source_over;
use crate::{BlendError, ImageFrame, Rgba8};

const CHANNELS: usize = 4;

/// A two-source transition shape rendered by [`render_transition`].
///
/// `W` and `H` are the frame width and height and `p` the progress.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TransitionShape {
    /// Columns `[0, floor(W * p))` show Preview and the rest Program
    /// (left to right).
    Wipe,
    /// Rows `[0, floor(H * p))` show Preview and the rest Program
    /// (top to bottom).
    VerticalWipe,
    /// Program moves left and Preview enters from the right by the exact
    /// integer offset `floor(W * p)`.
    Slide,
    /// Program moves up and Preview enters from the bottom by the exact
    /// integer offset `floor(H * p)`.
    VerticalSlide,
    /// Preview, nearest-neighbour scaled to `floor(W * p)` by `floor(H * p)`
    /// (each floored independently), replaces the centred box
    /// (left/top offsets floored) over a static Program.
    Zoom,
    /// Preview, scaled by `s = (1 + p) / 2` about its centre with that centre
    /// at `(W / 2 + (1 - p) * W, H / 2)`, flies in from the right, drawn over
    /// a static Program.
    Fly,
    /// [`Self::Fly`] with Preview also turned clockwise by
    /// `theta = (1 - p) * 180` degrees, as integer millidegrees
    /// `floor(180000 * (1 - p))`.
    FlyRotate,
    /// A cube of side `W` (faces `W` by `H`) turns about the vertical axis by
    /// `phi = p * 90` degrees (millidegrees `floor(90000 * p)`): Program on the
    /// front face turns away to the left while Preview on the right face turns
    /// in.
    ///
    /// The screen is the plane `z = 0` (`+z` into the screen) viewed in
    /// perspective from a camera at `(0, 0, -f)`, `f = 2W`; the cube centre is
    /// `(0, 0, W / 2)`. A face-local point `(x, z)` turns to
    /// `(x cos(phi) + z sin(phi), -x sin(phi) + z cos(phi))`. The front face
    /// is centred at local `(0, -W / 2)` with its horizontal axis along local
    /// `(1, 0)`; the right face at local `(W / 2, 0)` along `(0, 1)`. Each
    /// output pixel casts a ray through its screen point and intersects the
    /// plane of every face facing the camera (back faces are culled), giving
    /// face coordinates `(a, b)` sampled at texel
    /// `(a + W / 2 - 0.5, b + H / 2 - 0.5)`; pixels whose ray meets no
    /// facing plane are transparent `(0, 0, 0, 0)`.
    ///
    /// The visible faces of a convex cube never overlap, so when both face
    /// the camera exactly one draws each pixel: the front face where its
    /// `a < W / 2` (the ray passes left of the shared crease), the right face
    /// otherwise. Each samples with taps beyond the crease repeating the
    /// crease column (so the crease is a hard edge between two opaque
    /// pictures rather than a translucent seam of two half-covered fringes);
    /// every other face edge fades to transparent through the bilinear taps.
    Cube,
    /// [`Self::Cube`] with the cube centre pushed back by an extra depth
    /// `W * (1 - |2p - 1|)`, so the cube recedes and returns.
    CubeZoom,
    /// Program scaled by `1 + 2p` and Preview scaled by `3 - 2p`, both about
    /// the frame centre, crossfaded with weight `p` (the rounding of
    /// [`crate::crossfade`]).
    CrossZoom,
    /// Preview's left half (`L = floor(W / 2)` columns) slides in from the left
    /// edge and its right half from the right edge, meeting at the centre over
    /// a static Program: output columns `[0, floor(L * p))` show Preview
    /// columns `[L - floor(L * p), L)` and output columns
    /// `[W - floor((W - L) * p), W)` show Preview columns
    /// `[L, L + floor((W - L) * p))`.
    Merge,
}

/// The floating-point parameters of a geometric [`TransitionShape`] at one
/// progress value, shared by the CPU reference and the native GPU uniform.
///
/// Shapes that do not use a field leave it at its identity (`sin = 0`,
/// `cos = 1`, scales `1`, everything else `0`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransitionGeometry {
    /// Rotation sine: `FlyRotate`'s `theta` or `Cube`'s `phi`, from the Q30
    /// table divided by `2^30` (exact).
    pub sin: f64,
    /// Rotation cosine, as [`Self::sin`].
    pub cos: f64,
    /// Fly/FlyRotate Preview scale `(1 + p) / 2`, or `CrossZoom`'s Program
    /// scale `1 + 2p`.
    pub scale: f64,
    /// `CrossZoom`'s Preview scale `3 - 2p`.
    pub second_scale: f64,
    /// Fly/FlyRotate horizontal offset of Preview's centre, `(1 - p) * W`.
    pub offset_x: f64,
    /// Cube/CubeZoom cube-centre depth.
    pub depth: f64,
    /// Cube/CubeZoom camera distance `f = 2W`.
    pub camera: f64,
}

impl TransitionGeometry {
    const IDENTITY: Self = Self {
        sin: 0.0,
        cos: 1.0,
        scale: 1.0,
        second_scale: 1.0,
        offset_x: 0.0,
        depth: 0.0,
        camera: 0.0,
    };

    /// The parameters of `shape` on a frame `width` wide at progress
    /// `numerator / denominator`. A zero denominator or a numerator beyond it
    /// is treated as full progress.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn new(shape: TransitionShape, width: u32, numerator: u32, denominator: u32) -> Self {
        let (numerator, denominator) = if denominator == 0 || numerator > denominator {
            (1_u64, 1_u64)
        } else {
            (u64::from(numerator), u64::from(denominator))
        };
        let wide = u64::from(width);
        let width = f64::from(width);
        let remaining = denominator - numerator;
        let ratio = |top: u64, bottom: u64| top as f64 / bottom as f64;
        let trig = |millidegrees: u64| {
            let (sin, cos) = sin_cos_q30(i64::try_from(millidegrees).unwrap_or(0));
            let one = TRIG_ONE as f64;
            (sin as f64 / one, cos as f64 / one)
        };
        match shape {
            TransitionShape::Fly | TransitionShape::FlyRotate => {
                let (sin, cos) = if shape == TransitionShape::FlyRotate {
                    trig(180_000 * remaining / denominator)
                } else {
                    (0.0, 1.0)
                };
                Self {
                    sin,
                    cos,
                    scale: ratio(denominator + numerator, 2 * denominator),
                    offset_x: ratio(wide * remaining, denominator),
                    ..Self::IDENTITY
                }
            }
            TransitionShape::Cube | TransitionShape::CubeZoom => {
                let (sin, cos) = trig(90_000 * numerator / denominator);
                let extra = if shape == TransitionShape::CubeZoom {
                    // W * (1 - |2p - 1|) = W * (d - |2n - d|) / d.
                    ratio(
                        wide * (denominator - (2 * numerator).abs_diff(denominator)),
                        denominator,
                    )
                } else {
                    0.0
                };
                Self {
                    sin,
                    cos,
                    depth: width / 2.0 + extra,
                    camera: 2.0 * width,
                    ..Self::IDENTITY
                }
            }
            TransitionShape::CrossZoom => Self {
                scale: ratio(denominator + 2 * numerator, denominator),
                second_scale: ratio(3 * denominator - 2 * numerator, denominator),
                ..Self::IDENTITY
            },
            TransitionShape::Wipe
            | TransitionShape::VerticalWipe
            | TransitionShape::Slide
            | TransitionShape::VerticalSlide
            | TransitionShape::Zoom
            | TransitionShape::Merge => Self::IDENTITY,
        }
    }
}

/// `floor(size * numerator / denominator)`, the integer extent every
/// integer shape uses (wipe boundary, slide offset, zoom size, merge extent).
/// Clamped to `size`; a zero denominator yields `size`.
#[must_use]
pub fn transition_extent(size: u32, numerator: u32, denominator: u32) -> u32 {
    let extent = (u64::from(size) * u64::from(numerator))
        .checked_div(u64::from(denominator))
        .unwrap_or(u64::from(size))
        .min(u64::from(size));
    u32::try_from(extent).unwrap_or(size)
}

/// Renders `shape` from `from` (Program) toward `to` (Preview) at progress
/// `numerator / denominator`; see [`TransitionShape`] for each shape.
///
/// Both frames must be premultiplied RGBA8 with equal width, height, and
/// stride. Zero progress returns a clone of `from` and full progress a clone
/// of `to` for every shape. Row padding beyond the visible width is copied
/// from `from`.
///
/// # Errors
///
/// Returns a typed error for a zero denominator, a numerator beyond the
/// denominator, or frames whose width, height, or stride differ.
pub fn render_transition(
    shape: TransitionShape,
    from: &ImageFrame,
    to: &ImageFrame,
    numerator: u32,
    denominator: u32,
) -> Result<ImageFrame, BlendError> {
    validate_inputs(from, to, numerator, denominator)?;
    if numerator == 0 {
        return Ok(from.clone());
    }
    if numerator == denominator {
        return Ok(to.clone());
    }
    let mut pixels = from.pixels().to_vec();
    let progress = Progress {
        numerator,
        denominator,
    };
    match shape {
        TransitionShape::Wipe => wipe(&mut pixels, to, progress),
        TransitionShape::VerticalWipe => vertical_wipe(&mut pixels, to, progress),
        TransitionShape::Slide => slide(&mut pixels, from, to, progress),
        TransitionShape::VerticalSlide => vertical_slide(&mut pixels, from, to, progress),
        TransitionShape::Zoom => zoom(&mut pixels, to, progress),
        TransitionShape::Merge => merge(&mut pixels, to, progress),
        TransitionShape::Fly | TransitionShape::FlyRotate => {
            fly(
                &mut pixels,
                from,
                to,
                TransitionGeometry::new(shape, from.width(), numerator, denominator),
            );
        }
        TransitionShape::Cube | TransitionShape::CubeZoom => {
            cube(
                &mut pixels,
                from,
                to,
                TransitionGeometry::new(shape, from.width(), numerator, denominator),
            );
        }
        TransitionShape::CrossZoom => cross_zoom(&mut pixels, from, to, progress),
    }
    Ok(ImageFrame::new(
        from.width(),
        from.height(),
        from.stride(),
        pixels,
    )?)
}

#[derive(Clone, Copy)]
struct Progress {
    numerator: u32,
    denominator: u32,
}

impl Progress {
    fn extent(self, size: u32) -> u32 {
        transition_extent(size, self.numerator, self.denominator)
    }
}

fn texel_offset(stride: usize, x: u32, y: u32) -> usize {
    y as usize * stride + x as usize * CHANNELS
}

fn wipe(pixels: &mut [u8], to: &ImageFrame, progress: Progress) {
    let boundary = progress.extent(to.width()) as usize * CHANNELS;
    for (output_row, to_row) in pixels
        .chunks_exact_mut(to.stride())
        .zip(to.pixels().chunks_exact(to.stride()))
    {
        output_row[..boundary].copy_from_slice(&to_row[..boundary]);
    }
}

fn vertical_wipe(pixels: &mut [u8], to: &ImageFrame, progress: Progress) {
    let row_bytes = to.width() as usize * CHANNELS;
    let rows = progress.extent(to.height()) as usize;
    for (output_row, to_row) in pixels
        .chunks_exact_mut(to.stride())
        .zip(to.pixels().chunks_exact(to.stride()))
        .take(rows)
    {
        output_row[..row_bytes].copy_from_slice(&to_row[..row_bytes]);
    }
}

fn slide(pixels: &mut [u8], from: &ImageFrame, to: &ImageFrame, progress: Progress) {
    let offset = progress.extent(from.width()) as usize * CHANNELS;
    let row_bytes = from.width() as usize * CHANNELS;
    let remaining = row_bytes - offset;
    for ((output_row, from_row), to_row) in pixels
        .chunks_exact_mut(from.stride())
        .zip(from.pixels().chunks_exact(from.stride()))
        .zip(to.pixels().chunks_exact(to.stride()))
    {
        output_row[..remaining].copy_from_slice(&from_row[offset..row_bytes]);
        output_row[remaining..row_bytes].copy_from_slice(&to_row[..offset]);
    }
}

fn vertical_slide(pixels: &mut [u8], from: &ImageFrame, to: &ImageFrame, progress: Progress) {
    let height = from.height() as usize;
    let offset = progress.extent(from.height()) as usize;
    let remaining = height - offset;
    let row_bytes = from.width() as usize * CHANNELS;
    let stride = from.stride();
    for (y, output_row) in pixels.chunks_exact_mut(stride).enumerate() {
        let (source, row) = if y < remaining {
            (from, y + offset)
        } else {
            (to, y - remaining)
        };
        let start = row * stride;
        output_row[..row_bytes].copy_from_slice(&source.pixels()[start..start + row_bytes]);
    }
}

fn zoom(pixels: &mut [u8], to: &ImageFrame, progress: Progress) {
    let zoom_width = progress.extent(to.width());
    let zoom_height = progress.extent(to.height());
    if zoom_width == 0 || zoom_height == 0 {
        return;
    }
    let left = (to.width() - zoom_width) / 2;
    let top = (to.height() - zoom_height) / 2;
    let stride = to.stride();
    for output_y in 0..zoom_height {
        let source_y = u64::from(output_y) * u64::from(to.height()) / u64::from(zoom_height);
        // `source_y < height`, so it fits in `u32`.
        let source_y = u32::try_from(source_y).unwrap_or(u32::MAX);
        for output_x in 0..zoom_width {
            let source_x = u64::from(output_x) * u64::from(to.width()) / u64::from(zoom_width);
            let source_x = u32::try_from(source_x).unwrap_or(u32::MAX);
            let output = texel_offset(stride, left + output_x, top + output_y);
            let source = texel_offset(stride, source_x, source_y);
            pixels[output..output + CHANNELS]
                .copy_from_slice(&to.pixels()[source..source + CHANNELS]);
        }
    }
}

fn merge(pixels: &mut [u8], to: &ImageFrame, progress: Progress) {
    let width = to.width() as usize;
    let left_half = width / 2;
    let left = progress.extent(to.width() / 2) as usize;
    let right = progress.extent(to.width() - to.width() / 2) as usize;
    for (output_row, to_row) in pixels
        .chunks_exact_mut(to.stride())
        .zip(to.pixels().chunks_exact(to.stride()))
    {
        output_row[..left * CHANNELS]
            .copy_from_slice(&to_row[(left_half - left) * CHANNELS..left_half * CHANNELS]);
        output_row[(width - right) * CHANNELS..width * CHANNELS]
            .copy_from_slice(&to_row[left_half * CHANNELS..(left_half + right) * CHANNELS]);
    }
}

/// Which edge of a cube face is shared with the other visible face. Taps
/// beyond a shared edge repeat its column instead of turning transparent, so
/// the crease between the faces has no translucent seam.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Seam {
    None,
    /// The first column (Preview's face, right of the crease).
    Left,
    /// The last column (Program's face, left of the crease).
    Right,
}

/// Bilinear premultiplied sampling at a texel-centre coordinate, floored once
/// to Q8, transparent outside the frame.
struct Sampler<'a> {
    frame: &'a ImageFrame,
    width: i64,
    height: i64,
    limit_x: f64,
    limit_y: f64,
}

impl<'a> Sampler<'a> {
    fn new(frame: &'a ImageFrame) -> Self {
        Self {
            frame,
            width: i64::from(frame.width()),
            height: i64::from(frame.height()),
            limit_x: f64::from(frame.width()) * 256.0,
            limit_y: f64::from(frame.height()) * 256.0,
        }
    }

    fn texel(&self, x: i64, y: i64, seam: Seam) -> [u32; 4] {
        let x = match seam {
            Seam::None => x,
            Seam::Left => x.max(0),
            Seam::Right => x.min(self.width - 1),
        };
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return [0; 4];
        }
        // In range, so both fit in `u32`.
        let offset = texel_offset(
            self.frame.stride(),
            u32::try_from(x).unwrap_or(0),
            u32::try_from(y).unwrap_or(0),
        );
        let pixel = &self.frame.pixels()[offset..offset + CHANNELS];
        [
            u32::from(pixel[0]),
            u32::from(pixel[1]),
            u32::from(pixel[2]),
            u32::from(pixel[3]),
        ]
    }

    fn sample(&self, u: f64, v: f64) -> Rgba8 {
        self.sample_seam(u, v, Seam::None)
    }

    #[allow(clippy::cast_possible_truncation)]
    fn sample_seam(&self, u: f64, v: f64, seam: Seam) -> Rgba8 {
        let (u, v) = ((u * 256.0).floor(), (v * 256.0).floor());
        // Also rejects NaN.
        if !(u >= -256.0 && u < self.limit_x && v >= -256.0 && v < self.limit_y) {
            return Rgba8::new(0, 0, 0, 0);
        }
        // Integral and bounded by the frame, so the conversion is exact.
        let (u, v) = (u as i64, v as i64);
        let (column, row) = (u >> 8, v >> 8);
        let fx = u32::try_from(u & 255).unwrap_or(0);
        let fy = u32::try_from(v & 255).unwrap_or(0);
        let taps = [
            self.texel(column, row, seam),
            self.texel(column + 1, row, seam),
            self.texel(column, row + 1, seam),
            self.texel(column + 1, row + 1, seam),
        ];
        let mut channels = [0_u8; 4];
        for (channel, value) in channels.iter_mut().enumerate() {
            let top = taps[0][channel] * (256 - fx) + taps[1][channel] * fx;
            let bottom = taps[2][channel] * (256 - fx) + taps[3][channel] * fx;
            let mixed = (top * (256 - fy) + bottom * fy + 32_768) >> 16;
            *value = u8::try_from(mixed).unwrap_or(u8::MAX);
        }
        Rgba8::new(channels[0], channels[1], channels[2], channels[3])
    }
}

fn read(pixels: &[u8], offset: usize) -> Rgba8 {
    Rgba8::new(
        pixels[offset],
        pixels[offset + 1],
        pixels[offset + 2],
        pixels[offset + 3],
    )
}

/// Visits every visible pixel with its byte offset and centred screen
/// coordinates `(x + 0.5 - W / 2, y + 0.5 - H / 2)`.
fn for_each_pixel(frame: &ImageFrame, mut visit: impl FnMut(usize, f64, f64)) {
    let half_width = f64::from(frame.width()) / 2.0;
    let half_height = f64::from(frame.height()) / 2.0;
    for y in 0..frame.height() {
        let screen_y = f64::from(y) + 0.5 - half_height;
        for x in 0..frame.width() {
            let screen_x = f64::from(x) + 0.5 - half_width;
            visit(texel_offset(frame.stride(), x, y), screen_x, screen_y);
        }
    }
}

/// `(W - 1) / 2` and `(H - 1) / 2`: the texel coordinate of the frame centre.
fn texel_center(frame: &ImageFrame) -> (f64, f64) {
    (
        f64::from(frame.width() - 1) / 2.0,
        f64::from(frame.height() - 1) / 2.0,
    )
}

fn fly(pixels: &mut [u8], from: &ImageFrame, to: &ImageFrame, geometry: TransitionGeometry) {
    let sampler = Sampler::new(to);
    let (center_x, center_y) = texel_center(to);
    for_each_pixel(from, |offset, screen_x, screen_y| {
        let dx = screen_x - geometry.offset_x;
        let dy = screen_y;
        let a = (geometry.cos * dx + geometry.sin * dy) / geometry.scale;
        let b = (geometry.cos * dy - geometry.sin * dx) / geometry.scale;
        let preview = sampler.sample(a + center_x, b + center_y);
        let program = read(pixels, offset);
        pixels[offset..offset + CHANNELS]
            .copy_from_slice(&source_over(preview, program).to_bytes());
    });
}

/// One cube face in world `(x, z)` coordinates.
struct Face {
    center_x: f64,
    center_z: f64,
    axis_x: f64,
    axis_z: f64,
    normal_x: f64,
    normal_z: f64,
    /// `normal . (center - camera)`; negative when the face looks at the
    /// camera.
    facing: f64,
    camera: f64,
}

impl Face {
    fn new(center: (f64, f64), axis: (f64, f64), normal: (f64, f64), camera: f64) -> Self {
        Self {
            center_x: center.0,
            center_z: center.1,
            axis_x: axis.0,
            axis_z: axis.1,
            normal_x: normal.0,
            normal_z: normal.1,
            facing: center.0 * normal.0 + (camera + center.1) * normal.1,
            camera,
        }
    }

    /// The face coordinates `(a, b)` where the ray through screen point
    /// `(sx, sy)` meets the face's plane, or `None` for a back face or a ray
    /// that never reaches the plane.
    fn hit(&self, screen_x: f64, screen_y: f64) -> Option<(f64, f64)> {
        let denominator = screen_x * self.normal_x + self.camera * self.normal_z;
        // Both must be negative (a front face, a ray toward it); also
        // rejects NaN.
        if self.facing.partial_cmp(&0.0) != Some(core::cmp::Ordering::Less)
            || denominator.partial_cmp(&0.0) != Some(core::cmp::Ordering::Less)
        {
            return None;
        }
        let along = self.facing / denominator;
        let world_x = along * screen_x;
        let world_z = along * self.camera - self.camera;
        Some((
            (world_x - self.center_x) * self.axis_x + (world_z - self.center_z) * self.axis_z,
            along * screen_y,
        ))
    }
}

fn cube(pixels: &mut [u8], from: &ImageFrame, to: &ImageFrame, geometry: TransitionGeometry) {
    let TransitionGeometry {
        sin,
        cos,
        depth,
        camera,
        ..
    } = geometry;
    let half = f64::from(from.width()) / 2.0;
    let front = Face::new(
        (-half * sin, depth - half * cos),
        (cos, -sin),
        (-sin, -cos),
        camera,
    );
    let right = Face::new(
        (half * cos, depth - half * sin),
        (sin, cos),
        (cos, -sin),
        camera,
    );
    let (program, preview) = (Sampler::new(from), Sampler::new(to));
    let (center_x, center_y) = texel_center(from);
    for_each_pixel(from, |offset, screen_x, screen_y| {
        let color = match (front.hit(screen_x, screen_y), right.hit(screen_x, screen_y)) {
            // Both faces visible: the crease splits the screen, and the face
            // on the ray's side of it draws, extended across the crease.
            (Some((a, b)), Some((right_a, right_b))) => {
                if a < half {
                    program.sample_seam(a + center_x, b + center_y, Seam::Right)
                } else {
                    preview.sample_seam(right_a + center_x, right_b + center_y, Seam::Left)
                }
            }
            (Some((a, b)), None) => program.sample(a + center_x, b + center_y),
            (None, Some((a, b))) => preview.sample(a + center_x, b + center_y),
            (None, None) => Rgba8::new(0, 0, 0, 0),
        };
        pixels[offset..offset + CHANNELS].copy_from_slice(&color.to_bytes());
    });
}

fn cross_zoom(pixels: &mut [u8], from: &ImageFrame, to: &ImageFrame, progress: Progress) {
    let geometry = TransitionGeometry::new(
        TransitionShape::CrossZoom,
        from.width(),
        progress.numerator,
        progress.denominator,
    );
    let (program, preview) = (Sampler::new(from), Sampler::new(to));
    let (center_x, center_y) = texel_center(from);
    for_each_pixel(from, |offset, screen_x, screen_y| {
        let first = program.sample(
            screen_x / geometry.scale + center_x,
            screen_y / geometry.scale + center_y,
        );
        let second = preview.sample(
            screen_x / geometry.second_scale + center_x,
            screen_y / geometry.second_scale + center_y,
        );
        let blend =
            |left, right| blend_channel(left, right, progress.numerator, progress.denominator);
        let color = [
            blend(first.r, second.r),
            blend(first.g, second.g),
            blend(first.b, second.b),
            blend(first.a, second.a),
        ];
        pixels[offset..offset + CHANNELS].copy_from_slice(&color);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, seed: u8) -> ImageFrame {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let alpha = if (x + y) % 3 == 0 { 128 } else { 255 };
                let value = |offset: u32| {
                    u8::try_from((x * 17 + y * 29 + offset + u32::from(seed)) % 256)
                        .unwrap()
                        .min(alpha)
                };
                pixels.extend_from_slice(&[value(0), value(80), value(160), alpha]);
            }
        }
        ImageFrame::new(width, height, width as usize * 4, pixels).unwrap()
    }

    const SHAPES: [TransitionShape; 11] = [
        TransitionShape::Wipe,
        TransitionShape::VerticalWipe,
        TransitionShape::Slide,
        TransitionShape::VerticalSlide,
        TransitionShape::Zoom,
        TransitionShape::Fly,
        TransitionShape::FlyRotate,
        TransitionShape::Cube,
        TransitionShape::CubeZoom,
        TransitionShape::CrossZoom,
        TransitionShape::Merge,
    ];

    #[test]
    fn endpoints_are_byte_exact_clones() {
        let (from, to) = (frame(9, 7, 0), frame(9, 7, 90));
        for shape in SHAPES {
            assert_eq!(render_transition(shape, &from, &to, 0, 5).unwrap(), from);
            assert_eq!(render_transition(shape, &from, &to, 5, 5).unwrap(), to);
        }
    }

    #[test]
    fn invalid_inputs_are_typed_errors() {
        let (from, to) = (frame(4, 3, 0), frame(5, 3, 0));
        assert_eq!(
            render_transition(TransitionShape::Cube, &from, &from, 1, 0),
            Err(BlendError::ZeroDenominator)
        );
        assert_eq!(
            render_transition(TransitionShape::Fly, &from, &from, 3, 2),
            Err(BlendError::NumeratorExceedsDenominator {
                numerator: 3,
                denominator: 2
            })
        );
        assert_eq!(
            render_transition(TransitionShape::Merge, &from, &to, 1, 2),
            Err(BlendError::WidthMismatch { left: 4, right: 5 })
        );
    }

    #[test]
    fn geometry_is_exact_where_the_rationals_are() {
        let fly = TransitionGeometry::new(TransitionShape::FlyRotate, 10, 1, 2);
        assert_eq!((fly.scale, fly.offset_x), (0.75, 5.0));
        // 90 degrees exactly.
        assert_eq!((fly.sin, fly.cos), (1.0, 0.0));
        let cube = TransitionGeometry::new(TransitionShape::CubeZoom, 10, 1, 2);
        assert_eq!((cube.depth, cube.camera), (15.0, 20.0));
        let cross = TransitionGeometry::new(TransitionShape::CrossZoom, 10, 1, 4);
        assert_eq!((cross.scale, cross.second_scale), (1.5, 2.5));
        assert_eq!(transition_extent(7, 2, 3), 4);
        assert_eq!(transition_extent(7, 1, 0), 7);
    }

    #[test]
    fn tiny_progress_cube_is_the_program() {
        // floor(90000 / 100000) = 0 millidegrees: the front face is the screen.
        let (from, to) = (frame(9, 7, 0), frame(9, 7, 90));
        assert_eq!(
            render_transition(TransitionShape::Cube, &from, &to, 1, 100_000).unwrap(),
            from
        );
    }
}
