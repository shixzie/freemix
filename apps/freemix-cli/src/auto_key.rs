//! `scene-layer-auto-key` and `remote-scene-layer-auto-key` (SW-008): one
//! command that samples a layer input's picture and sets the layer's whole
//! chroma key from it with [`fm_ui_model::auto_key`], the sampler Studio's
//! Auto key button and eyedropper use.
//!
//! Locally the picture is the layer's simulated input rendered at the
//! project size, or a binary PPM (`--picture`); the key is stored like
//! `scene-layer-key`. Remotely the picture is a PPM the operator saved and
//! the key is sent as one `scene_layer_key_set` edit, so the daemon
//! padlocks, journals, and can undo it like any key edit.

use std::path::Path;

use fm_model::{Project, SourceRef};
use fm_types::SceneId;
use fm_ui_model::auto_key::{AutoKey, AutoKeyBacking, AutoKeyPicture, AutoKeySample, auto_key};

use crate::app::{AppFailure, AppResult};

/// A straight RGBA8 picture, row-major and tightly packed.
pub(crate) struct Picture {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

impl Picture {
    /// Runs the auto key on this picture.
    pub(crate) fn auto_key(&self, sample: AutoKeySample) -> AppResult<AutoKey> {
        let picture = AutoKeyPicture::new(self.width, self.height, &self.rgba)?;
        Ok(auto_key(picture, sample)?)
    }
}

/// The sample a command asks for: a point pick or the dominant backing.
pub(crate) fn sample(point: Option<(u32, u32)>) -> AutoKeySample {
    point.map_or(AutoKeySample::Dominant, |(x, y)| AutoKeySample::Point {
        x,
        y,
    })
}

/// Reads a binary RGB PPM (`P6`, maximum value 255, `#` comments allowed
/// in the header) such as `render` writes.
pub(crate) fn read_ppm(path: &Path) -> AppResult<Picture> {
    let bytes = std::fs::read(path)
        .map_err(|error| AppFailure(format!("cannot read {}: {error}", path.display())))?;
    parse_ppm(&bytes)
        .ok_or_else(|| {
            AppFailure(format!(
                "{} is not a binary PPM (P6, maximum 255)",
                path.display()
            ))
        })
        .map_err(Into::into)
}

fn parse_ppm(bytes: &[u8]) -> Option<Picture> {
    let mut position = 0;
    let mut token = || -> Option<&[u8]> {
        loop {
            while bytes.get(position)?.is_ascii_whitespace() {
                position += 1;
            }
            if bytes[position] == b'#' {
                while *bytes.get(position)? != b'\n' {
                    position += 1;
                }
                continue;
            }
            let start = position;
            while bytes
                .get(position)
                .is_some_and(|byte| !byte.is_ascii_whitespace())
            {
                position += 1;
            }
            return Some(&bytes[start..position]);
        }
    };
    if token()? != b"P6" {
        return None;
    }
    let mut number = || -> Option<u32> { core::str::from_utf8(token()?).ok()?.parse().ok() };
    let width = number()?;
    let height = number()?;
    if number()? != 255 || width == 0 || height == 0 {
        return None;
    }
    // One whitespace byte ends the header.
    let start = position + 1;
    let length = usize::try_from(u64::from(width) * u64::from(height) * 3).ok()?;
    let pixels = bytes.get(start..start.checked_add(length)?)?;
    let mut rgba = Vec::with_capacity(length / 3 * 4);
    for pixel in pixels.as_chunks::<3>().0 {
        rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], u8::MAX]);
    }
    Some(Picture {
        width,
        height,
        rgba,
    })
}

/// Layer `index` of `scene`'s input picture: its simulated input rendered
/// at the project size.
pub(crate) fn layer_input_picture(
    project: &Project,
    scene: SceneId,
    index: usize,
) -> AppResult<Picture> {
    let layer = project
        .scenes()
        .iter()
        .find(|candidate| candidate.id == scene)
        .ok_or_else(|| AppFailure(format!("scene {scene} does not exist")))?
        .layers
        .get(index)
        .ok_or_else(|| AppFailure(format!("scene {scene} has no layer {index}")))?;
    let SourceRef::Input(input) = layer.source else {
        return Err(AppFailure(
            "the layer shows a scene, which has no single input picture; pass --picture".into(),
        )
        .into());
    };
    let dimensions = project.settings().video.dimensions;
    let pipeline =
        crate::app::simulated_pipeline(project, dimensions.width(), dimensions.height())?;
    let frame = pipeline.render_source(input, 0).map_err(|_| {
        AppFailure(format!(
            "input {input} has no simulated picture to sample; pass --picture <file.ppm>"
        ))
    })?;
    let mut rgba = Vec::with_capacity(
        usize::try_from(u64::from(frame.width()) * u64::from(frame.height()) * 4).unwrap_or(0),
    );
    for y in 0..frame.height() {
        for x in 0..frame.width() {
            let pixel = frame
                .pixel(x, y)
                .ok_or_else(|| AppFailure("rendered input frame is malformed".into()))?;
            rgba.extend_from_slice(&[pixel.r, pixel.g, pixel.b, pixel.a]);
        }
    }
    Ok(Picture {
        width: frame.width(),
        height: frame.height(),
        rgba,
    })
}

/// One line describing what an auto key derived.
pub(crate) fn describe(auto: &AutoKey) -> String {
    let backing = match auto.backing {
        AutoKeyBacking::Green => "green",
        AutoKeyBacking::Blue => "blue",
        AutoKeyBacking::Other => "other",
    };
    let despill = match auto.despill {
        fm_protocol::WireKeyDespill::Off => "off".to_owned(),
        fm_protocol::WireKeyDespill::Green { strength } => format!("green:{strength}"),
        fm_protocol::WireKeyDespill::Blue { strength } => format!("blue:{strength}"),
    };
    format!(
        "auto key: backing={backing} colour={},{},{} similarity={} smoothness={} spill={} \
         despill={despill} samples={} agreeing={}",
        auto.red,
        auto.green,
        auto.blue,
        auto.similarity,
        auto.smoothness,
        auto.spill,
        auto.samples,
        auto.agreeing
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppm_headers_with_comments_parse_and_bad_ones_do_not() {
        let mut bytes = b"P6\n# a comment\n2 1\n255\n".to_vec();
        bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        let picture = parse_ppm(&bytes).unwrap();
        assert_eq!((picture.width, picture.height), (2, 1));
        assert_eq!(picture.rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
        assert!(parse_ppm(b"P5\n1 1\n255\n\0").is_none());
        assert!(parse_ppm(b"P6\n1 1\n65535\n\0\0\0\0\0\0").is_none());
        assert!(parse_ppm(b"P6\n2 2\n255\n\0\0\0").is_none());
        assert!(parse_ppm(b"P6\n0 2\n255\n").is_none());
    }
}
