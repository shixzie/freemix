use crate::{FrameError, ImageFrame, Rgba8};

/// Generates a packed solid-color frame.
///
/// # Errors
///
/// Returns a [`FrameError`] when the requested dimensions cannot form a
/// bounded frame.
pub fn solid_color(width: u32, height: u32, color: Rgba8) -> Result<ImageFrame, FrameError> {
    let mut frame = ImageFrame::packed(width, height)?;
    for pixel in frame.pixels_mut().as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&color.to_bytes());
    }
    Ok(frame)
}
