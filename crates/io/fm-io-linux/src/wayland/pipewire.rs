//! The `PipeWire` stream of a granted share (IN-011).
//!
//! [`stream`] links an input stream to the shared node on the `PipeWire`
//! connection the portal opened ([`fm_io_pipewire::VideoStream`], which
//! loads libpipewire at run time) and offers raw video in the 4-byte
//! layouts [`PixelLayout`] reads, at any size and rate. No DMA-BUF
//! modifiers are offered, so the compositor copies into shared memory
//! buffers. The stream is linked to that node only: when the node goes
//! away the stream fails instead of being relinked to another source, and
//! a stream the desktop pauses is reported paused until it resumes.
//! Between pictures the stop check, the portal's session check,
//! and the rate cap's idle call run; only the newest picture of each pass
//! is read.

use std::os::fd::OwnedFd;
use std::time::Duration;

use fm_io_pipewire::pod::video;
use fm_io_pipewire::{StreamEnd, VideoStream};

use super::convert::{PixelLayout, SourcePicture};
use super::{ShareEnd, ShareSink};

/// How long one pass waits for the stream.
const PASS: Duration = Duration::from_millis(50);

/// The formats offered, the first preferred.
const OFFERED: [u32; 8] = [
    video::BGRX,
    video::RGBX,
    video::XRGB,
    video::XBGR,
    video::BGRA,
    video::RGBA,
    video::ARGB,
    video::ABGR,
];

/// The layout a negotiated format's bytes have.
fn layout(format: u32) -> Option<PixelLayout> {
    match format {
        video::BGRX | video::BGRA => Some(PixelLayout::Bgrx),
        video::RGBX | video::RGBA => Some(PixelLayout::Rgbx),
        video::XRGB | video::ARGB => Some(PixelLayout::Xrgb),
        video::XBGR | video::ABGR => Some(PixelLayout::Xbgr),
        _ => None,
    }
}

/// Reads the shared stream of `node` on the `remote` connection into
/// `sink` until the share ends; `check` runs between pictures and ends the
/// share when it says so.
pub fn stream(
    remote: OwnedFd,
    node: u32,
    sink: &mut dyn ShareSink,
    check: &mut dyn FnMut() -> Option<ShareEnd>,
) -> ShareEnd {
    let stream = match VideoStream::connect(remote, node, c"freemix-screen-capture", &OFFERED) {
        Ok(stream) => stream,
        Err(error) => return ShareEnd::Failed(format!("PipeWire: {error}")),
    };
    loop {
        let update = stream.wait(PASS);
        match update.end {
            Some(StreamEnd::Ended(reason)) => return ShareEnd::Ended(reason),
            Some(StreamEnd::Failed(reason)) => {
                return ShareEnd::Failed(format!("PipeWire: {reason}"));
            }
            None => {}
        }
        if sink.stopping() {
            return ShareEnd::Stopped;
        }
        if let Some(end) = check() {
            return end;
        }
        match update.streaming {
            Some(true) => sink.streaming(),
            Some(false) => sink.paused(),
            None => {}
        }
        if let Some(frame) = update.frame {
            if let Some(layout) = layout(frame.format) {
                sink.frame(&SourcePicture {
                    data: &frame.data,
                    width: frame.width,
                    height: frame.height,
                    stride: frame.stride,
                    layout,
                });
            }
            stream.recycle(frame);
        }
        sink.idle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_format_maps_to_a_layout() {
        assert!(OFFERED.iter().all(|format| layout(*format).is_some()));
        assert_eq!(layout(video::BGRX), Some(PixelLayout::Bgrx));
        assert_eq!(layout(video::RGBA), Some(PixelLayout::Rgbx));
        assert_eq!(layout(video::XRGB), Some(PixelLayout::Xrgb));
        assert_eq!(layout(video::ABGR), Some(PixelLayout::Xbgr));
        // I420.
        assert_eq!(layout(2), None);
    }
}
