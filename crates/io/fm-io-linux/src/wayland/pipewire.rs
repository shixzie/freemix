//! The `PipeWire` stream of a granted share (IN-011; `--cfg
//! freemix_pipewire` builds only).
//!
//! [`stream`] connects to the `PipeWire` remote the portal opened, links an
//! input stream to the shared node, and offers raw video in the 4-byte
//! layouts [`PixelLayout`] reads, at any size and rate. No DMA-BUF
//! modifiers are offered, so the compositor copies into shared memory
//! buffers, which are read in place. The loop is iterated by hand so the
//! stop check, the portal's session check, and the rate cap's idle call
//! run between pictures; of the buffers queued since the last pass only
//! the newest is read.

use std::cell::RefCell;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::time::Duration;

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::buffer::{ChunkFlags, DataType};
use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use spa::param::video::{VideoFormat, VideoInfoRaw};
use spa::param::{ParamType, format_utils};
use spa::pod::Pod;
use spa::utils::{Direction, Fraction, Rectangle, SpaTypes};

use super::convert::{PixelLayout, SourcePicture};
use super::{ShareEnd, ShareSink};

/// How long one loop pass waits for the stream.
const PASS: Duration = Duration::from_millis(50);

#[derive(Default)]
struct Shared {
    format: Option<(PixelLayout, u32, u32)>,
    end: Option<ShareEnd>,
    streaming: bool,
}

/// The layout a negotiated format's bytes have.
fn layout(format: VideoFormat) -> Option<PixelLayout> {
    match format {
        VideoFormat::BGRx | VideoFormat::BGRA => Some(PixelLayout::Bgrx),
        VideoFormat::RGBx | VideoFormat::RGBA => Some(PixelLayout::Rgbx),
        VideoFormat::xRGB | VideoFormat::ARGB => Some(PixelLayout::Xrgb),
        VideoFormat::xBGR | VideoFormat::ABGR => Some(PixelLayout::Xbgr),
        _ => None,
    }
}

/// The raw video formats offered, as an `EnumFormat` pod.
fn offer() -> Result<Vec<u8>, String> {
    let object = spa::pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::RGBx,
            VideoFormat::xRGB,
            VideoFormat::xBGR,
            VideoFormat::BGRA,
            VideoFormat::RGBA,
            VideoFormat::ARGB,
            VideoFormat::ABGR,
        ),
        spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            Rectangle {
                width: 1920,
                height: 1080
            },
            Rectangle {
                width: 1,
                height: 1
            },
            Rectangle {
                width: 16_384,
                height: 16_384
            }
        ),
        spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 30, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction {
                num: 1000,
                denom: 1
            }
        ),
    );
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map(|(cursor, _)| cursor.into_inner())
    .map_err(|error| format!("the video format offer cannot be built: {error:?}"))
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
    match run(remote, node, sink, check) {
        Ok(end) => end,
        Err(error) => ShareEnd::Failed(format!("PipeWire: {error}")),
    }
}

fn run(
    remote: OwnedFd,
    node: u32,
    sink: &mut dyn ShareSink,
    check: &mut dyn FnMut() -> Option<ShareEnd>,
) -> Result<ShareEnd, String> {
    pw::init();
    let failed = |what: &'static str| move |error: pw::Error| format!("{what}: {error}");
    let mainloop =
        pw::main_loop::MainLoopRc::new(None).map_err(failed("no main loop can be created"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(failed("no context can be created"))?;
    let core = context
        .connect_fd_rc(remote, None)
        .map_err(failed("the desktop's PipeWire connection cannot be used"))?;
    let shared = Rc::new(RefCell::new(Shared::default()));

    let on_error = Rc::clone(&shared);
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _seq, _result, message| {
            if id == pw::core::PW_ID_CORE {
                on_error.borrow_mut().end = Some(ShareEnd::Failed(format!(
                    "the PipeWire connection failed: {message}"
                )));
            }
        })
        .register();

    let stream = pw::stream::StreamBox::new(
        &core,
        "freemix-screen-capture",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(failed("no stream can be created"))?;
    let _stream_listener = stream
        .add_local_listener_with_user_data(Rc::clone(&shared))
        .state_changed(|_, shared, _old, new| match new {
            pw::stream::StreamState::Error(message) => {
                shared.borrow_mut().end = Some(ShareEnd::Failed(format!(
                    "the shared stream failed: {message}"
                )));
            }
            pw::stream::StreamState::Streaming => shared.borrow_mut().streaming = true,
            // Only reached again after connecting: the stream went away.
            pw::stream::StreamState::Unconnected => {
                shared.borrow_mut().end = Some(ShareEnd::Ended("the shared stream ended".into()));
            }
            _ => {}
        })
        .param_changed(|_, shared, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Ok((MediaType::Video, MediaSubtype::Raw)) = format_utils::parse_format(param)
            else {
                return;
            };
            let mut info = VideoInfoRaw::default();
            if info.parse(param).is_err() {
                return;
            }
            let size = info.size();
            shared.borrow_mut().format =
                layout(info.format()).map(|layout| (layout, size.width, size.height));
        })
        .register()
        .map_err(failed("the stream cannot be watched"))?;

    let offer = offer()?;
    let offer = Pod::from_bytes(&offer).ok_or("the video format offer is malformed")?;
    stream
        .connect(
            Direction::Input,
            Some(node),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut [offer],
        )
        .map_err(failed("the shared stream cannot be connected"))?;

    loop {
        mainloop.loop_().iterate(pw::loop_::Timeout::Finite(PASS));
        if let Some(end) = shared.borrow_mut().end.take() {
            return Ok(end);
        }
        if sink.stopping() {
            return Ok(ShareEnd::Stopped);
        }
        if let Some(end) = check() {
            return Ok(end);
        }
        if std::mem::take(&mut shared.borrow_mut().streaming) {
            sink.streaming();
        }
        let format = shared.borrow().format;
        read_newest(&stream, format, sink);
        sink.idle();
    }
}

/// Shows the newest buffer queued on `stream` in `format`, if any; older
/// buffers go back as they are dropped.
fn read_newest(
    stream: &pw::stream::Stream,
    format: Option<(PixelLayout, u32, u32)>,
    sink: &mut dyn ShareSink,
) {
    let mut newest = None;
    while let Some(buffer) = stream.dequeue_buffer() {
        newest = Some(buffer);
    }
    let (Some(mut buffer), Some((layout, width, height))) = (newest, format) else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    if !matches!(data.type_(), DataType::MemPtr | DataType::MemFd) {
        return;
    }
    let chunk = data.chunk();
    let offset = usize::try_from(chunk.offset()).unwrap_or(usize::MAX);
    let size = usize::try_from(chunk.size()).unwrap_or(0);
    let stride = usize::try_from(chunk.stride())
        .ok()
        .filter(|stride| *stride > 0)
        .unwrap_or(usize::try_from(width).unwrap_or(0) * 4);
    if chunk.flags().contains(ChunkFlags::CORRUPTED) || size == 0 {
        return;
    }
    let Some(bytes) = data
        .data()
        .and_then(|bytes| bytes.get(offset..offset.checked_add(size)?))
    else {
        return;
    };
    sink.frame(&SourcePicture {
        data: bytes,
        width,
        height,
        stride,
        layout,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiated_formats_map_to_layouts() {
        assert_eq!(layout(VideoFormat::BGRx), Some(PixelLayout::Bgrx));
        assert_eq!(layout(VideoFormat::RGBA), Some(PixelLayout::Rgbx));
        assert_eq!(layout(VideoFormat::xRGB), Some(PixelLayout::Xrgb));
        assert_eq!(layout(VideoFormat::ABGR), Some(PixelLayout::Xbgr));
        assert_eq!(layout(VideoFormat::I420), None);
        assert!(offer().is_ok_and(|offer| Pod::from_bytes(&offer).is_some()));
    }
}
