//! The real V4L2 probe and device provider (Linux only).
//!
//! [`SystemV4l2`] opens each `/dev/video*` node with the pure-Rust
//! `linuxvideo` binding and fills a [`RawNode`]: `VIDIOC_QUERYCAP` (driver,
//! card, bus info, device capabilities), `VIDIOC_ENUM_FMT`,
//! `VIDIOC_ENUM_FRAMESIZES` (a stepwise range becomes the standard sizes it
//! allows, see [`stepwise_sizes`]), `VIDIOC_ENUM_FRAMEINTERVALS` per size,
//! `VIDIOC_G_FMT` (current size and field order), and the first input's
//! status (no signal / no power). Querying never streams, so it is safe
//! while another process captures from the node.

use std::panic::{AssertUnwindSafe, catch_unwind};

use linuxvideo::format::{Format, FrameIntervals, FrameSizes};
use linuxvideo::{BufType, CapabilityFlags, Device, InputStatus};

use crate::probe::{RawFormat, RawIntervals, RawNode, RawSize, field, stepwise_sizes};

/// Probes the real V4L2 nodes of this system.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemV4l2;

impl SystemV4l2 {
    /// The `/dev/video*` node paths.
    #[must_use]
    pub fn node_paths(&self) -> Vec<String> {
        let mut paths = std::fs::read_dir("/dev")
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter_map(|entry| {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let number = name.strip_prefix("video")?;
                        (!number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
                            .then(|| format!("/dev/{name}"))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        paths.sort();
        paths
    }

    /// Probes one node.
    ///
    /// # Errors
    ///
    /// Returns why the node cannot be opened or queried (a driver that
    /// makes the binding panic is reported as an error too).
    pub fn probe(&self, path: &str) -> Result<RawNode, String> {
        catch_unwind(AssertUnwindSafe(|| probe_node(path)))
            .unwrap_or_else(|_| Err(format!("{path}: the driver reported malformed data")))
    }
}

fn probe_node(path: &str) -> Result<RawNode, String> {
    let device = Device::open(path).map_err(|error| format!("{path}: {error}"))?;
    let capabilities = device
        .capabilities()
        .map_err(|error| format!("{path}: {error}"))?;
    let capture = capabilities
        .device_capabilities()
        .contains(CapabilityFlags::VIDEO_CAPTURE);
    let mut node = RawNode {
        path: path.to_owned(),
        driver: capabilities.driver().to_owned(),
        card: capabilities.card().to_owned(),
        bus_info: capabilities.bus_info().to_owned(),
        capture,
        formats: Vec::new(),
        current: None,
        no_signal: false,
    };
    if !capture {
        return Ok(node);
    }
    for description in device.formats(BufType::VIDEO_CAPTURE) {
        let Ok(description) = description else {
            break;
        };
        let pixel_format = description.pixel_format();
        let Ok(sizes) = device.frame_sizes(pixel_format) else {
            continue;
        };
        let sizes = match sizes {
            FrameSizes::Discrete(list) => list
                .iter()
                .map(|size| (size.width(), size.height()))
                .collect::<Vec<_>>(),
            FrameSizes::Stepwise(range) | FrameSizes::Continuous(range) => stepwise_sizes(
                (range.min_width(), range.max_width(), range.step_width()),
                (range.min_height(), range.max_height(), range.step_height()),
            ),
        };
        let sizes = sizes
            .into_iter()
            .filter_map(|(width, height)| {
                let intervals = match device.frame_intervals(pixel_format, width, height).ok()? {
                    FrameIntervals::Discrete(list) => RawIntervals::Discrete(
                        list.iter()
                            .map(|interval| {
                                (interval.fract().numerator(), interval.fract().denominator())
                            })
                            .collect(),
                    ),
                    FrameIntervals::Stepwise(range) | FrameIntervals::Continuous(range) => {
                        RawIntervals::Range {
                            min: (range.min().numerator(), range.min().denominator()),
                            max: (range.max().numerator(), range.max().denominator()),
                        }
                    }
                };
                Some(RawSize {
                    width,
                    height,
                    intervals,
                })
            })
            .collect();
        node.formats.push(RawFormat {
            fourcc: pixel_format.as_fourcc(),
            sizes,
        });
    }
    if let Ok(Format::VideoCapture(format)) = device.format(BufType::VIDEO_CAPTURE) {
        node.current = Some((format.width(), format.height(), debug_field(&format)));
    }
    if let Some(Ok(input)) = device.inputs().next() {
        node.no_signal = input
            .status()
            .intersects(InputStatus::NO_SIGNAL | InputStatus::NO_POWER);
    }
    Ok(node)
}

/// The field order of a current format. The binding exposes it only in
/// the format's `Debug` text (`field: INTERLACED_TB`).
fn debug_field(format: &impl std::fmt::Debug) -> u32 {
    let text = format!("{format:?}");
    let Some(value) = text
        .split("field: ")
        .nth(1)
        .and_then(|rest| rest.split([',', ' ', '}']).next())
    else {
        return field::ANY;
    };
    match value {
        "NONE" => field::NONE,
        "TOP" => field::TOP,
        "BOTTOM" => field::BOTTOM,
        "INTERLACED" => field::INTERLACED,
        "SEQ_TB" => field::SEQ_TB,
        "SEQ_BT" => field::SEQ_BT,
        "ALTERNATE" => field::ALTERNATE,
        "INTERLACED_TB" => field::INTERLACED_TB,
        "INTERLACED_BT" => field::INTERLACED_BT,
        _ => field::ANY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_orders_are_read_from_debug_text() {
        struct Text(&'static str);
        impl std::fmt::Debug for Text {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.0)
            }
        }
        assert_eq!(
            debug_field(&Text(
                "PixFormat { width: 1920, field: INTERLACED_TB, bytesperline: 3840 }"
            )),
            field::INTERLACED_TB
        );
        assert_eq!(debug_field(&Text("PixFormat { field: NONE }")), field::NONE);
        assert_eq!(debug_field(&Text("PixFormat { }")), field::ANY);
    }
}
