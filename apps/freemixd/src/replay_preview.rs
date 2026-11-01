//! Exact Replay multiview snapshots (OR-014).
//!
//! A `replay_preview_request` asks the authority for one Replay input's
//! multiview at its player's current position: the input and its linked A/B
//! partner, then up to eight recorded angles of the cued event, each decoded
//! from its exact segment frame and composed in an adaptive grid. A native daemon decodes the picture out of the
//! segment files off the session loop; a simulated daemon draws one dark
//! cell per angle because it has no segment media. The answer is one
//! four-channel QOI stream of straight RGBA8 pixels at the requested size
//! (at most 640x360) split into `replay_preview_frame` parts, through the
//! same latest-wins preview worker, lossy slot, and per-session preview rate
//! as title and scene previews.

use fm_auth::Policy;
use fm_control::ControlService;
use fm_engine::{EngineReplayMultiview, EngineReplayTile};
use fm_frame::ClockDomainId;
use fm_protocol::{
    ReplayPreviewFrameMessage, ReplayPreviewRequest, ServerIdentity, StructuredError,
    encode_qoi_rgba, title_preview_parts,
};
use fm_types::{FrameRate, InputId};
use fm_video::ImageFrame;

use crate::title_preview::refusal;

/// Most tiles one preview composes: eight angles plus channels A and B.
const MAX_PREVIEW_TILES: usize = 10;

/// The grid columns for `tiles`: one angle fills the canvas, two to four
/// make a 2x2 quad, and more make a four-column grid (eight angles are
/// 4x2).
const fn preview_columns(tiles: usize) -> usize {
    match tiles {
        0 | 1 => 1,
        2..=4 => 2,
        _ => 4,
    }
}

/// The picture size and rate of the show, for decoding replay segments.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(feature = "native-media"), allow(dead_code))]
pub(crate) struct PreviewVideoFormat {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rate: FrameRate,
    pub(crate) clock_domain: ClockDomainId,
}

/// One exact replay multiview, resolved on the session loop and rendered
/// off it.
pub(crate) struct ReplayPreviewJob {
    server: ServerIdentity,
    pub(crate) request_id: String,
    wire_input: fm_protocol::WireInputId,
    width: u16,
    height: u16,
    multiview: EngineReplayMultiview,
}

/// Resolves the replay multiview `request` asks for.
///
/// # Errors
///
/// A `not_cued` refusal when the Replay input has nothing cued.
pub(crate) fn prepare_replay_preview(
    control: &ControlService<Policy>,
    server: &ServerIdentity,
    request: &ReplayPreviewRequest,
) -> Result<ReplayPreviewJob, StructuredError> {
    let input = request.input.to_domain();
    let multiview = control
        .replay_multiview(input)
        .ok_or_else(|| refusal("not_cued", format!("Replay input {input} has nothing cued")))?;
    Ok(ReplayPreviewJob {
        server: server.clone(),
        request_id: request.request_id.clone(),
        wire_input: request.input,
        width: request.width,
        height: request.height,
        multiview,
    })
}

impl ReplayPreviewJob {
    /// Renders the grid and splits it into `replay_preview_frame` parts.
    ///
    /// # Errors
    ///
    /// A `render_failed` refusal when the picture cannot be composed or
    /// encoded.
    pub(crate) fn render(
        &self,
        format: Option<PreviewVideoFormat>,
    ) -> Result<Vec<ReplayPreviewFrameMessage>, StructuredError> {
        let rgba = self.composite(format);
        let qoi = encode_qoi_rgba(self.width, self.height, &rgba)
            .map_err(|error| refusal("render_failed", error.to_string()))?;
        Ok(title_preview_parts(
            &self.server,
            &self.request_id,
            self.wire_input,
            0,
            false,
            self.width,
            self.height,
            &qoi,
        ))
    }

    /// The adaptive grid: the channel tiles first, then the angles, each
    /// scaled into its cell; empty cells stay black and a source with no
    /// decoded picture gets a dark tinted cell.
    fn composite(&self, format: Option<PreviewVideoFormat>) -> Vec<u8> {
        let width = usize::from(self.width);
        let height = usize::from(self.height);
        let mut pixels = vec![0_u8; width * height * 4];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = u8::MAX;
        }
        let tiles: Vec<&EngineReplayTile> = self
            .multiview
            .channels
            .iter()
            .chain(self.multiview.tiles.iter())
            .take(MAX_PREVIEW_TILES)
            .collect();
        let columns = preview_columns(tiles.len());
        let rows = tiles.len().div_ceil(columns);
        for (index, tile) in tiles.into_iter().enumerate() {
            let (column, row) = (index % columns, index / columns);
            let left = column * width / columns;
            let top = row * height / rows;
            let right = (column + 1) * width / columns;
            let bottom = (row + 1) * height / rows;
            let cell = (right - left, bottom - top);
            match decode_tile(format, tile) {
                Some(frame) => blit(&mut pixels, (width, height), (left, top), cell, &frame),
                // A live-edge angle has no closed segment yet: it stays
                // black. A missing decode or a simulated daemon gets a
                // tinted cell so the grid stays legible.
                None if format.is_some() && tile.location.is_none() => {}
                None => tint_cell(&mut pixels, (width, height), (left, top), cell, tile.angle),
            }
        }
        pixels
    }
}

/// Decodes `tile`'s exact segment frame; `None` without native media, at
/// the live edge (no closed segment), or when the file cannot be decoded.
fn decode_tile(format: Option<PreviewVideoFormat>, tile: &EngineReplayTile) -> Option<ImageFrame> {
    let format = format?;
    let location = tile.location.as_ref()?;
    #[cfg(feature = "native-media")]
    {
        let mut decoder =
            crate::native_replay::SegmentDecoder::new(crate::native_replay::ReplayFormat {
                width: format.width,
                height: format.height,
                rate: format.rate,
                clock_domain: format.clock_domain,
            });
        let frame = decoder.decode(&location.path, location.frame)?;
        fm_compositor::image_from_cpu_frame(&frame).ok()
    }
    #[cfg(not(feature = "native-media"))]
    {
        let _ = (format, location);
        None
    }
}

/// Scales `frame` into the cell at `(left, top)` and copies it into
/// `pixels` (tight RGBA8, `canvas` sized).
fn blit(
    pixels: &mut [u8],
    canvas: (usize, usize),
    at: (usize, usize),
    cell: (usize, usize),
    frame: &ImageFrame,
) {
    let Ok(width) = u32::try_from(cell.0) else {
        return;
    };
    let Ok(height) = u32::try_from(cell.1) else {
        return;
    };
    let Ok(scaled) = fm_video::scale_nearest(frame, width, height) else {
        return;
    };
    let (canvas_width, _) = canvas;
    for row in 0..cell.1 {
        let source = &scaled.pixels()[row * cell.0 * 4..(row + 1) * cell.0 * 4];
        let start = ((at.1 + row) * canvas_width + at.0) * 4;
        pixels[start..start + source.len()].copy_from_slice(source);
    }
}

/// Fills one cell with a dark grey tinted by the angle, so a simulated
/// daemon still shows the grid shape and which angles are present.
fn tint_cell(
    pixels: &mut [u8],
    canvas: (usize, usize),
    at: (usize, usize),
    cell: (usize, usize),
    angle: InputId,
) {
    let tint = 24_u8.saturating_add((angle.get().get() % 48) as u8);
    let (canvas_width, _) = canvas;
    for row in 0..cell.1 {
        for column in 0..cell.0 {
            let start = ((at.1 + row) * canvas_width + at.0 + column) * 4;
            pixels[start..start + 4].copy_from_slice(&[
                tint,
                tint,
                tint.saturating_add(8),
                u8::MAX,
            ]);
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;
    use fm_engine::{EngineReplayMultiview, EngineReplayTile};
    use fm_protocol::TitlePreviewAssembler;

    /// Protocol 2.127: eight angles fill a four-column, two-row grid.
    #[test]
    fn eight_angles_compose_a_four_by_two_grid() {
        let input = |value: u128| InputId::new(NonZeroU128::new(value).unwrap());
        let tiles = (1..=8)
            .map(|value| EngineReplayTile {
                angle: input(value),
                source_frame: 0,
                location: None,
            })
            .collect();
        let job = ReplayPreviewJob {
            server: ServerIdentity {
                engine_id: "e".into(),
                project_id: "p".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            request_id: "replay-preview-grid".into(),
            wire_input: fm_protocol::WireInputId::from_domain(input(9)),
            width: 320,
            height: 180,
            multiview: EngineReplayMultiview {
                input: input(9),
                event: 1,
                source_frame: 0,
                channels: Vec::new(),
                tiles,
            },
        };
        let parts = job.render(None).unwrap();
        let mut assembler = TitlePreviewAssembler::new();
        let mut image = None;
        for part in parts {
            image = assembler.push(&part).unwrap().or(image);
        }
        let image = image.expect("the grid assembled");
        assert_eq!((image.width, image.height), (320, 180));
        // Four columns by two rows: every cell is tinted (not black) and the
        // eight cells do not leave an empty grid row.
        for row in 0..2 {
            for column in 0..4 {
                let x = column * 80 + 40;
                let y = row * 90 + 45;
                let at = (usize::try_from(y * 320 + x).unwrap()) * 4;
                let pixel = &image.rgba[at..at + 4];
                assert!(
                    pixel[0] > 16 || pixel[1] > 16 || pixel[2] > 16,
                    "cell {row},{column}: {pixel:?}"
                );
            }
        }
    }
}
