//! Placement of one picture inside a differently sized output raster.
//!
//! [`OutputScaling`] names the four operator-visible policies a display or
//! other raster output can apply when its picture and target sizes differ.
//! [`OutputScaling::viewport`] reduces each policy to one signed destination
//! rectangle: target pixels inside the rectangle sample the picture with
//! center-nearest mapping, and pixels outside it are black. A rectangle may
//! extend past the target (Fill and oversized 1:1 crop) or leave black bars
//! (Fit and undersized 1:1).

use std::{fmt, str::FromStr};

/// How a picture is placed in a target raster of a different size.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum OutputScaling {
    /// Preserve aspect ratio and show the whole picture, with black bars.
    #[default]
    Fit,
    /// Preserve aspect ratio and cover the whole target, cropping overflow.
    Fill,
    /// Scale each axis independently to exactly cover the target.
    Stretch,
    /// Map one picture pixel to one target pixel, centered; crops or borders.
    OneToOne,
}

impl OutputScaling {
    /// Every policy in a stable order.
    pub const ALL: [Self; 4] = [Self::Fit, Self::Fill, Self::Stretch, Self::OneToOne];

    /// The canonical lowercase token (`fit`, `fill`, `stretch`, `1:1`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fit => "fit",
            Self::Fill => "fill",
            Self::Stretch => "stretch",
            Self::OneToOne => "1:1",
        }
    }

    /// Returns the destination rectangle for a `source` picture placed in a
    /// `target` raster, or `None` when any dimension is zero.
    ///
    /// Fit is identical to the established centered letterbox/pillarbox
    /// placement: the fitted axis is floored and the bars are split with the
    /// odd pixel on the far side. Fill covers the target with the overflowing
    /// axis rounded up so no target pixel is left unsampled.
    #[must_use]
    pub fn viewport(
        self,
        source_width: u32,
        source_height: u32,
        target_width: u32,
        target_height: u32,
    ) -> Option<ScaledViewport> {
        if source_width == 0 || source_height == 0 || target_width == 0 || target_height == 0 {
            return None;
        }
        let (sw, sh, tw, th) = (
            u64::from(source_width),
            u64::from(source_height),
            u64::from(target_width),
            u64::from(target_height),
        );
        // True when the target is at most as wide (relative to its height) as the source.
        let target_is_narrower = tw * sh <= th * sw;
        let (width, height) = match self {
            Self::Stretch => (tw, th),
            Self::OneToOne => (sw, sh),
            Self::Fit if target_is_narrower => (tw, ((tw * sh) / sw).max(1)),
            Self::Fit => (((th * sw) / sh).max(1), th),
            Self::Fill if target_is_narrower => ((th * sw).div_ceil(sh), th),
            Self::Fill => (tw, (tw * sh).div_ceil(sw)),
        };
        let width = u32::try_from(width).ok()?;
        let height = u32::try_from(height).ok()?;
        Some(ScaledViewport {
            x: centered_origin(target_width, width),
            y: centered_origin(target_height, height),
            width,
            height,
        })
    }
}

fn centered_origin(target: u32, extent: u32) -> i32 {
    let offset = (i64::from(target) - i64::from(extent)) / 2;
    // Both values are u32, so the halved difference always fits i32.
    i32::try_from(offset).unwrap_or(if offset < 0 { i32::MIN } else { i32::MAX })
}

impl fmt::Display for OutputScaling {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An unknown scaling token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseOutputScalingError(pub String);

impl fmt::Display for ParseOutputScalingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown scaling `{}`; expected fit, fill, stretch, or 1:1",
            self.0
        )
    }
}

impl std::error::Error for ParseOutputScalingError {}

impl FromStr for OutputScaling {
    type Err = ParseOutputScalingError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "fit" => Ok(Self::Fit),
            "fill" => Ok(Self::Fill),
            "stretch" => Ok(Self::Stretch),
            "1:1" | "one-to-one" => Ok(Self::OneToOne),
            other => Err(ParseOutputScalingError(other.to_owned())),
        }
    }
}

/// A signed destination rectangle in target pixels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ScaledViewport {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl ScaledViewport {
    /// Returns the picture pixel sampled by target pixel `(x, y)` using
    /// center-nearest mapping, or `None` for a black (outside) pixel.
    #[must_use]
    pub fn source_pixel(
        self,
        source_width: u32,
        source_height: u32,
        x: u32,
        y: u32,
    ) -> Option<(u32, u32)> {
        let local_x = i64::from(x) - i64::from(self.x);
        let local_y = i64::from(y) - i64::from(self.y);
        if local_x < 0
            || local_y < 0
            || local_x >= i64::from(self.width)
            || local_y >= i64::from(self.height)
        {
            return None;
        }
        Some((
            center_nearest(local_x.unsigned_abs(), source_width, self.width),
            center_nearest(local_y.unsigned_abs(), source_height, self.height),
        ))
    }
}

fn center_nearest(coordinate: u64, source: u32, destination: u32) -> u32 {
    let value = ((2 * coordinate + 1) * u64::from(source)) / (2 * u64::from(destination));
    u32::try_from(value.min(u64::from(source.saturating_sub(1)))).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_letterboxes_and_pillarboxes_with_the_established_split() {
        assert_eq!(
            OutputScaling::Fit.viewport(4, 1, 6, 5),
            Some(ScaledViewport {
                x: 0,
                y: 2,
                width: 6,
                height: 1
            })
        );
        assert_eq!(
            OutputScaling::Fit.viewport(4, 1, 10, 1),
            Some(ScaledViewport {
                x: 3,
                y: 0,
                width: 4,
                height: 1
            })
        );
    }

    #[test]
    fn fill_covers_every_target_pixel_and_crops_the_overflowing_axis() {
        let viewport = OutputScaling::Fill
            .viewport(1920, 1080, 1080, 1920)
            .unwrap();
        assert_eq!(viewport.height, 1920);
        assert_eq!(viewport.width, 3414);
        assert_eq!(viewport.x, -1167);
        for (x, y) in [(0, 0), (1079, 1919), (540, 960)] {
            assert!(viewport.source_pixel(1920, 1080, x, y).is_some());
        }
        let wide = OutputScaling::Fill.viewport(4, 3, 16, 9).unwrap();
        assert_eq!((wide.width, wide.height, wide.x, wide.y), (16, 12, 0, -1));
    }

    #[test]
    fn stretch_and_one_to_one_are_exact() {
        assert_eq!(
            OutputScaling::Stretch.viewport(1280, 720, 1920, 1200),
            Some(ScaledViewport {
                x: 0,
                y: 0,
                width: 1920,
                height: 1200
            })
        );
        let one = OutputScaling::OneToOne
            .viewport(1280, 720, 1920, 1080)
            .unwrap();
        assert_eq!((one.x, one.y, one.width, one.height), (320, 180, 1280, 720));
        assert_eq!(one.source_pixel(1280, 720, 320, 180), Some((0, 0)));
        assert_eq!(one.source_pixel(1280, 720, 319, 180), None);
        let crop = OutputScaling::OneToOne
            .viewport(3840, 2160, 1920, 1080)
            .unwrap();
        assert_eq!((crop.x, crop.y), (-960, -540));
        assert_eq!(crop.source_pixel(3840, 2160, 0, 0), Some((960, 540)));
    }

    #[test]
    fn zero_dimensions_have_no_viewport_and_tokens_round_trip() {
        assert_eq!(OutputScaling::Fit.viewport(0, 1, 1, 1), None);
        assert_eq!(OutputScaling::Fill.viewport(1, 1, 1, 0), None);
        for scaling in OutputScaling::ALL {
            assert_eq!(scaling.as_str().parse::<OutputScaling>(), Ok(scaling));
        }
        assert!("zoom".parse::<OutputScaling>().is_err());
    }
}
