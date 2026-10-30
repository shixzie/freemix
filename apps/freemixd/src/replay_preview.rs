//! Exact Replay multiview snapshots (OR-014).
//!
//! A `replay_preview_request` asks the authority for one Replay input's
//! four-angle view at its player's current position: up to four recorded
//! angles of the cued event, each decoded from its exact segment frame and
//! composed as a 2x2 grid. A native daemon decodes the picture out of the
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

/// The picture size and rate of the show, for decoding replay segments.
#[derive(Clone, Copy, Debug)]
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

    /// The 2x2 grid: the first four angles in row-major order, each scaled
    /// into its cell; empty cells stay black and an angle with no decoded
    /// picture gets a dark tinted cell.
    fn composite(&self, format: Option<PreviewVideoFormat>) -> Vec<u8> {
        let width = usize::from(self.width);
        let height = usize::from(self.height);
        let mut pixels = vec![0_u8; width * height * 4];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = u8::MAX;
        }
        let (cell_width, cell_height) = (width / 2, height / 2);
        for (index, tile) in self.multiview.tiles.iter().take(4).enumerate() {
            let (column, row) = (index % 2, index / 2);
            let left = column * cell_width;
            let top = row * cell_height;
            match decode_tile(format, tile) {
                Some(frame) => blit(
                    &mut pixels,
                    (width, height),
                    (left, top),
                    (cell_width, cell_height),
                    &frame,
                ),
                None => tint_cell(
                    &mut pixels,
                    (width, height),
                    (left, top),
                    (cell_width, cell_height),
                    tile.angle,
                ),
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
