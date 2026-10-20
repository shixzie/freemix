//! Shared desktop pictures into the program's format (IN-011).
//!
//! A Wayland desktop shares 4-byte pixels in one of the `BGRx`, `RGBx`,
//! `xRGB`, or `xBGR` byte orders (and their alpha twins, whose alpha a
//! screen capture ignores). [`letterbox`] cuts the cropped region out of
//! one shared picture and fits it into the program picture as opaque RGBA,
//! bilinearly scaled with its aspect ratio kept and black bars around it,
//! exactly as the X11 capture's `ffmpeg` letterbox does.

/// The byte order of a shared picture's 4-byte pixels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PixelLayout {
    /// Blue, green, red, then padding or alpha (`BGRx`, `BGRA`).
    Bgrx,
    /// Red, green, blue, then padding or alpha (`RGBx`, `RGBA`).
    Rgbx,
    /// Padding or alpha, then red, green, blue (`xRGB`, `ARGB`).
    Xrgb,
    /// Padding or alpha, then blue, green, red (`xBGR`, `ABGR`).
    Xbgr,
}

impl PixelLayout {
    /// Every layout, in the order a capture prefers them.
    pub const ALL: [Self; 4] = [Self::Bgrx, Self::Rgbx, Self::Xrgb, Self::Xbgr];

    /// The byte offsets of red, green, and blue within a pixel.
    #[must_use]
    pub const fn offsets(self) -> [usize; 3] {
        match self {
            Self::Bgrx => [2, 1, 0],
            Self::Rgbx => [0, 1, 2],
            Self::Xrgb => [1, 2, 3],
            Self::Xbgr => [3, 2, 1],
        }
    }

    /// The layout's name.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Bgrx => "BGRx",
            Self::Rgbx => "RGBx",
            Self::Xrgb => "xRGB",
            Self::Xbgr => "xBGR",
        }
    }
}

/// One shared picture: `height` rows of `width` pixels, `stride` bytes
/// apart.
#[derive(Clone, Copy, Debug)]
pub struct SourcePicture<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub layout: PixelLayout,
}

impl SourcePicture<'_> {
    /// Whether every row the size and stride name lies in the data.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        let (Ok(width), Ok(height)) = (usize::try_from(self.width), usize::try_from(self.height))
        else {
            return false;
        };
        let Some(row) = width.checked_mul(4) else {
            return false;
        };
        width > 0
            && height > 0
            && self.stride >= row
            && self
                .stride
                .checked_mul(height - 1)
                .and_then(|start| start.checked_add(row))
                .is_some_and(|end| end <= self.data.len())
    }
}

/// One output axis: for each output position, the two source positions it
/// blends and the second's weight (0 to 256).
fn taps(source: u32, output: u32) -> Vec<(usize, usize, u32)> {
    let source = u64::from(source);
    let output = u64::from(output).max(1);
    let last = source.saturating_sub(1) * 256;
    (0..output)
        .map(|index| {
            // The output pixel's centre, in 1/256 source pixels.
            let centre = ((2 * index + 1) * source * 256) / (2 * output);
            let position = centre.saturating_sub(128).min(last);
            let first = usize::try_from(position >> 8).unwrap_or(usize::MAX);
            let second = (first + 1).min(usize::try_from(source.saturating_sub(1)).unwrap_or(0));
            (first, second, u32::try_from(position & 0xff).unwrap_or(0))
        })
        .collect()
}

/// The `region` (`x`, `y`, `width`, `height` in source pixels) of `source`
/// fitted into an opaque RGBA picture of `width` x `height`: scaled with
/// its aspect ratio kept, centred, with black bars. `None` when the source
/// is incomplete, the region does not lie in it, or a size is zero.
#[must_use]
pub fn letterbox(
    source: &SourcePicture<'_>,
    region: (u32, u32, u32, u32),
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let (region_x, region_y, region_width, region_height) = region;
    if !source.is_complete()
        || width == 0
        || height == 0
        || region_width == 0
        || region_height == 0
        || region_x.checked_add(region_width)? > source.width
        || region_y.checked_add(region_height)? > source.height
    {
        return None;
    }
    let out_width = usize::try_from(width).ok()?;
    let out_height = usize::try_from(height).ok()?;
    let mut picture = vec![0_u8; out_width.checked_mul(out_height)?.checked_mul(4)?];
    for pixel in picture.as_chunks_mut::<4>().0 {
        pixel[3] = u8::MAX;
    }
    // The fitted size: the whole width or the whole height, the other side
    // by the region's aspect ratio, at least one pixel.
    let (fit_width, fit_height) = if u64::from(region_width) * u64::from(height)
        >= u64::from(region_height) * u64::from(width)
    {
        let fitted = (u64::from(region_height) * u64::from(width) + u64::from(region_width) / 2)
            / u64::from(region_width);
        (width, u32::try_from(fitted).ok()?.clamp(1, height))
    } else {
        let fitted = (u64::from(region_width) * u64::from(height) + u64::from(region_height) / 2)
            / u64::from(region_height);
        (u32::try_from(fitted).ok()?.clamp(1, width), height)
    };
    let left = usize::try_from((width - fit_width) / 2).ok()?;
    let top = usize::try_from((height - fit_height) / 2).ok()?;
    let [red, green, blue] = source.layout.offsets();
    let origin_x = usize::try_from(region_x).ok()?;
    let origin_y = usize::try_from(region_y).ok()?;
    let at = |x: usize, y: usize| (origin_y + y) * source.stride + (origin_x + x) * 4;
    let out_row = out_width * 4;
    if fit_width == region_width && fit_height == region_height {
        // One to one: reorder the bytes.
        for y in 0..usize::try_from(fit_height).ok()? {
            let row = &mut picture[(top + y) * out_row + left * 4..];
            for x in 0..usize::try_from(fit_width).ok()? {
                let from = at(x, y);
                let pixel = &mut row[x * 4..x * 4 + 3];
                pixel[0] = source.data[from + red];
                pixel[1] = source.data[from + green];
                pixel[2] = source.data[from + blue];
            }
        }
        return Some(picture);
    }
    let columns = taps(region_width, fit_width);
    let rows = taps(region_height, fit_height);
    for (y, &(row_a, row_b, weight_y)) in rows.iter().enumerate() {
        let row = &mut picture[(top + y) * out_row + left * 4..];
        for (x, &(column_a, column_b, weight_x)) in columns.iter().enumerate() {
            let corners = [
                at(column_a, row_a),
                at(column_b, row_a),
                at(column_a, row_b),
                at(column_b, row_b),
            ];
            for (channel, offset) in [red, green, blue].into_iter().enumerate() {
                let sample = |corner: usize| u32::from(source.data[corners[corner] + offset]);
                let upper = sample(0) * (256 - weight_x) + sample(1) * weight_x;
                let lower = sample(2) * (256 - weight_x) + sample(3) * weight_x;
                let value = (upper * (256 - weight_y) + lower * weight_y + 32_768) >> 16;
                row[x * 4 + channel] = u8::try_from(value).unwrap_or(u8::MAX);
            }
        }
    }
    Some(picture)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `width` x `height` picture in `layout`, each pixel `colour(x, y)`
    /// (RGB), rows padded to `stride`.
    fn picture(
        width: u32,
        height: u32,
        stride: usize,
        layout: PixelLayout,
        colour: impl Fn(u32, u32) -> [u8; 3],
    ) -> Vec<u8> {
        let mut data = vec![0xee_u8; stride * height as usize];
        let [red, green, blue] = layout.offsets();
        for y in 0..height {
            for x in 0..width {
                let at = y as usize * stride + x as usize * 4;
                let [r, g, b] = colour(x, y);
                data[at + red] = r;
                data[at + green] = g;
                data[at + blue] = b;
            }
        }
        data
    }

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let at = (y * width + x) as usize * 4;
        [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
    }

    #[test]
    fn every_layout_reads_as_opaque_rgba_one_to_one() {
        for layout in PixelLayout::ALL {
            let data = picture(4, 2, 20, layout, |x, y| {
                [
                    u8::try_from(x * 10).unwrap(),
                    u8::try_from(y * 50).unwrap(),
                    200,
                ]
            });
            let source = SourcePicture {
                data: &data,
                width: 4,
                height: 2,
                stride: 20,
                layout,
            };
            let rgba = letterbox(&source, (0, 0, 4, 2), 4, 2).unwrap();
            assert_eq!(pixel(&rgba, 4, 3, 1), [30, 50, 200, 255], "{layout:?}");
            assert_eq!(pixel(&rgba, 4, 0, 0), [0, 0, 200, 255], "{layout:?}");
        }
    }

    #[test]
    fn a_crop_is_cut_out_and_letterboxed_with_black_bars() {
        // A 16x8 red picture with a 4x4 green square at (8, 2).
        let data = picture(16, 8, 64, PixelLayout::Bgrx, |x, y| {
            if (8..12).contains(&x) && (2..6).contains(&y) {
                [0, 255, 0]
            } else {
                [255, 0, 0]
            }
        });
        let source = SourcePicture {
            data: &data,
            width: 16,
            height: 8,
            stride: 64,
            layout: PixelLayout::Bgrx,
        };
        // The square alone, into a 16x8 picture: 8x8 in the middle.
        let rgba = letterbox(&source, (8, 2, 4, 4), 16, 8).unwrap();
        assert_eq!(pixel(&rgba, 16, 0, 4), [0, 0, 0, 255], "left bar");
        assert_eq!(pixel(&rgba, 16, 15, 4), [0, 0, 0, 255], "right bar");
        for (x, y) in [(4, 0), (8, 4), (11, 7)] {
            assert_eq!(pixel(&rgba, 16, x, y), [0, 255, 0, 255], "({x}, {y})");
        }
        // The whole picture halved keeps its colours away from the edge.
        let half = letterbox(&source, (0, 0, 16, 8), 8, 4).unwrap();
        assert_eq!(pixel(&half, 8, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&half, 8, 4, 2)[1], 255);
        // A tall region fills the height with bars at the sides.
        let tall = letterbox(&source, (0, 0, 4, 8), 16, 8).unwrap();
        assert_eq!(pixel(&tall, 16, 1, 4), [0, 0, 0, 255]);
        assert_eq!(pixel(&tall, 16, 8, 4), [255, 0, 0, 255]);
    }

    #[test]
    fn incomplete_pictures_and_outside_regions_are_refused() {
        let data = vec![0_u8; 4 * 4 * 4];
        let source = |stride, height| SourcePicture {
            data: &data,
            width: 4,
            height,
            stride,
            layout: PixelLayout::Rgbx,
        };
        assert!(letterbox(&source(16, 4), (0, 0, 4, 4), 8, 8).is_some());
        assert!(letterbox(&source(16, 5), (0, 0, 4, 4), 8, 8).is_none());
        assert!(letterbox(&source(12, 4), (0, 0, 4, 4), 8, 8).is_none());
        assert!(letterbox(&source(16, 4), (2, 0, 4, 4), 8, 8).is_none());
        assert!(letterbox(&source(16, 4), (0, 0, 0, 4), 8, 8).is_none());
        assert!(letterbox(&source(16, 4), (0, 0, 4, 4), 0, 8).is_none());
    }
}
