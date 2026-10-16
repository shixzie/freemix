//! V4L2 node probes, their parsing into capture devices, stable keys, and
//! device resolution.
//!
//! A [`RawNode`] is what the V4L2 ioctls report for one `/dev/video*` node
//! (`VIDIOC_QUERYCAP`, `VIDIOC_ENUM_FMT`, `VIDIOC_ENUM_FRAMESIZES`,
//! `VIDIOC_ENUM_FRAMEINTERVALS`, `VIDIOC_G_FMT`, and the current input's
//! status), already expanded so stepwise sizes are a candidate list. It is
//! plain data so enumeration can be tested from fixtures; the real probe
//! lives in [`crate::v4l2`] and only fills these structures.
//!
//! [`devices_from_nodes`] keeps the nodes that capture video (a UVC camera
//! also exposes a metadata node), maps their pixel formats (YUYV, NV12, and
//! MJPEG; others are skipped), sizes, and frame intervals to
//! [`CaptureFormat`]s, marks interlaced sizes from the reported field order,
//! and gives each device its stable key.
//!
//! A stable key is `v4l2:<bus info>#<card>` for the first capture node on a
//! bus and `v4l2:<bus info>@<n>#<card>` for the `n`th (`n` from 2). The bus
//! info is the physical port, so the key survives a replug into the same
//! port and a renumbered `/dev/video*` node; [`resolve`] falls back to the
//! card name when the device moved to another port, and accepts a plain
//! node path as a key.

use fm_types::FrameRate;
use fm_types::capture::{
    CaptureDevice, CaptureFormat, CapturePixelFormat, CaptureScan, MAX_CAPTURE_DEVICE_FORMATS,
    MAX_CAPTURE_DIMENSION, MAX_CAPTURE_FPS, MAX_CAPTURE_TEXT_BYTES, MIN_CAPTURE_DIMENSION,
    bounded_capture_text,
};

/// The stable key prefix of V4L2 devices.
pub const V4L2_KEY_PREFIX: &str = "v4l2:";

/// V4L2 field orders (`enum v4l2_field`).
pub mod field {
    pub const ANY: u32 = 0;
    pub const NONE: u32 = 1;
    pub const TOP: u32 = 2;
    pub const BOTTOM: u32 = 3;
    pub const INTERLACED: u32 = 4;
    pub const SEQ_TB: u32 = 5;
    pub const SEQ_BT: u32 = 6;
    pub const ALTERNATE: u32 = 7;
    pub const INTERLACED_TB: u32 = 8;
    pub const INTERLACED_BT: u32 = 9;
}

/// The frame intervals one size offers, as seconds-per-frame fractions
/// `(numerator, denominator)`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RawIntervals {
    Discrete(Vec<(u32, u32)>),
    /// A stepwise or continuous range, shortest interval first.
    Range {
        min: (u32, u32),
        max: (u32, u32),
    },
}

/// One frame size of one pixel format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawSize {
    pub width: u32,
    pub height: u32,
    pub intervals: RawIntervals,
}

/// One pixel format of a node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawFormat {
    pub fourcc: [u8; 4],
    pub sizes: Vec<RawSize>,
}

/// What one V4L2 node reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawNode {
    /// The node path, such as `/dev/video0`.
    pub path: String,
    pub driver: String,
    pub card: String,
    pub bus_info: String,
    /// Whether the node captures video (`V4L2_CAP_VIDEO_CAPTURE` in its
    /// device capabilities).
    pub capture: bool,
    pub formats: Vec<RawFormat>,
    /// The current format's size and field order (`VIDIOC_G_FMT`).
    pub current: Option<(u32, u32, u32)>,
    /// The current input reports no signal or no power.
    pub no_signal: bool,
}

/// A capture device as enumerated, with its signal flag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProbedDevice {
    pub info: CaptureDevice,
    /// The device reports no input signal (a capture card without a
    /// source).
    pub no_signal: bool,
}

/// The parts of a V4L2 stable key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct V4l2Key {
    pub bus: String,
    /// One-based position among the capture nodes on the bus.
    pub ordinal: u32,
    pub card: String,
}

impl V4l2Key {
    /// The key text.
    #[must_use]
    pub fn text(&self) -> String {
        if self.ordinal <= 1 {
            format!("{V4L2_KEY_PREFIX}{}#{}", self.bus, self.card)
        } else {
            format!(
                "{V4L2_KEY_PREFIX}{}@{}#{}",
                self.bus, self.ordinal, self.card
            )
        }
    }

    /// Parses a key made by [`Self::text`].
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        let rest = key.strip_prefix(V4L2_KEY_PREFIX)?;
        let (location, card) = rest.split_once('#')?;
        let (bus, ordinal) = match location.rsplit_once('@') {
            Some((bus, ordinal))
                if !ordinal.is_empty() && ordinal.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                (bus, ordinal.parse::<u32>().ok().filter(|n| *n >= 2)?)
            }
            _ => (location, 1),
        };
        (!bus.is_empty() && !card.is_empty()).then(|| Self {
            bus: bus.to_owned(),
            ordinal,
            card: card.to_owned(),
        })
    }
}

/// Key-safe text: bounded, without control characters, and without the
/// `#` and `@` key separators.
fn key_text(text: &str) -> String {
    bounded_capture_text(text, MAX_CAPTURE_TEXT_BYTES)
        .chars()
        .map(|c| if matches!(c, '#' | '@') { '_' } else { c })
        .collect()
}

/// Standard frame rates offered from a continuous or stepwise interval
/// range.
const STANDARD_RATES: [(u32, u32); 10] = [
    (24_000, 1_001),
    (24, 1),
    (25, 1),
    (30_000, 1_001),
    (30, 1),
    (50, 1),
    (60_000, 1_001),
    (60, 1),
    (100, 1),
    (120, 1),
];

/// Standard picture sizes offered from a continuous or stepwise size range.
pub const STANDARD_SIZES: [(u32, u32); 12] = [
    (640, 360),
    (640, 480),
    (720, 480),
    (720, 576),
    (960, 540),
    (1024, 576),
    (1280, 720),
    (1600, 900),
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
    (4096, 2160),
];

/// The standard sizes within a stepwise range (`min..=max` with `step`),
/// plus the range's largest size.
#[must_use]
pub fn stepwise_sizes(
    (min_width, max_width, step_width): (u32, u32, u32),
    (min_height, max_height, step_height): (u32, u32, u32),
) -> Vec<(u32, u32)> {
    let fits = |value: u32, min: u32, max: u32, step: u32| {
        (min..=max).contains(&value) && (step <= 1 || (value - min).is_multiple_of(step))
    };
    let mut sizes = STANDARD_SIZES
        .iter()
        .copied()
        .filter(|&(width, height)| {
            fits(width, min_width, max_width, step_width)
                && fits(height, min_height, max_height, step_height)
        })
        .collect::<Vec<_>>();
    let largest = (
        max_width.min(MAX_CAPTURE_DIMENSION),
        max_height.min(MAX_CAPTURE_DIMENSION),
    );
    if largest.0 >= min_width && largest.1 >= min_height && !sizes.contains(&largest) {
        sizes.push(largest);
    }
    sizes
}

/// The frame rates of `intervals`: each discrete interval inverted, or the
/// standard rates within a range plus its fastest rate.
#[must_use]
pub fn interval_rates(intervals: &RawIntervals) -> Vec<FrameRate> {
    let rate = |(numerator, denominator): (u32, u32)| FrameRate::new(denominator, numerator).ok();
    let mut rates = match intervals {
        RawIntervals::Discrete(list) => list.iter().copied().filter_map(rate).collect::<Vec<_>>(),
        RawIntervals::Range { min, max } => {
            let fastest = rate(*min);
            let slowest = rate(*max);
            let millihertz = |rate: FrameRate| {
                u64::from(rate.numerator()) * 1_000 / u64::from(rate.denominator())
            };
            let mut rates = Vec::new();
            if let (Some(fastest), Some(slowest)) = (fastest, slowest) {
                let range = millihertz(slowest)..=millihertz(fastest);
                rates.extend(
                    STANDARD_RATES
                        .iter()
                        .filter_map(|&(n, d)| FrameRate::new(n, d).ok())
                        .filter(|rate| range.contains(&millihertz(*rate))),
                );
                rates.push(fastest);
            }
            rates
        }
    };
    rates.retain(|rate| {
        u64::from(rate.numerator()) <= u64::from(MAX_CAPTURE_FPS) * u64::from(rate.denominator())
    });
    let mut unique = Vec::with_capacity(rates.len());
    for rate in rates {
        if !unique.contains(&rate) {
            unique.push(rate);
        }
    }
    unique
}

/// The scan of pictures `height` lines tall reported with V4L2 `field`.
///
/// `V4L2_FIELD_INTERLACED` leaves the order to the video standard: NTSC
/// (480 lines) sends the bottom field first, everything else the top.
#[must_use]
pub const fn field_scan(field: u32, height: u32) -> CaptureScan {
    match field {
        field::INTERLACED_BT | field::SEQ_BT => CaptureScan::InterlacedBff,
        field::INTERLACED | field::ALTERNATE if height == 480 || height == 486 => {
            CaptureScan::InterlacedBff
        }
        field::INTERLACED_TB | field::SEQ_TB | field::INTERLACED | field::ALTERNATE => {
            CaptureScan::InterlacedTff
        }
        _ => CaptureScan::Progressive,
    }
}

/// The capture formats a node offers, deduplicated, ordered by pixel
/// format, then largest picture and fastest rate first, and bounded to
/// [`MAX_CAPTURE_DEVICE_FORMATS`].
#[must_use]
pub fn node_formats(node: &RawNode) -> Vec<CaptureFormat> {
    let dimension = MIN_CAPTURE_DIMENSION..=MAX_CAPTURE_DIMENSION;
    let mut formats = Vec::new();
    for raw in &node.formats {
        let Some(pixel_format) = CapturePixelFormat::from_fourcc(raw.fourcc) else {
            continue;
        };
        for size in &raw.sizes {
            if !dimension.contains(&size.width) || !dimension.contains(&size.height) {
                continue;
            }
            let scan = match node.current {
                Some((width, height, field)) if width == size.width && height == size.height => {
                    field_scan(field, height)
                }
                // A node reporting interlaced fields without a matching
                // current size interlaces every size.
                Some((_, _, field)) if node.formats.len() == 1 && raw.sizes.len() == 1 => {
                    field_scan(field, size.height)
                }
                _ => CaptureScan::Progressive,
            };
            for rate in interval_rates(&size.intervals) {
                let format = CaptureFormat {
                    width: size.width,
                    height: size.height,
                    rate,
                    pixel_format,
                    scan,
                };
                if format.validate().is_ok() && !formats.contains(&format) {
                    formats.push(format);
                }
            }
        }
    }
    formats.sort_by(|a: &CaptureFormat, b: &CaptureFormat| {
        let area = |format: &CaptureFormat| u64::from(format.width) * u64::from(format.height);
        a.pixel_format
            .cmp(&b.pixel_format)
            .then(area(b).cmp(&area(a)))
            .then(b.millihertz().cmp(&a.millihertz()))
            .then(a.scan.cmp(&b.scan))
            .then(a.token().cmp(&b.token()))
    });
    formats.truncate(MAX_CAPTURE_DEVICE_FORMATS);
    formats
}

/// The capture devices of `nodes`: every node that captures video in one
/// of the supported pixel formats, with its stable key and the audio device
/// `audio` pairs with it. Devices are ordered by stable key.
pub fn devices_from_nodes(
    nodes: &[RawNode],
    mut audio: impl FnMut(&RawNode) -> Option<String>,
) -> Vec<ProbedDevice> {
    let mut sorted = nodes.iter().filter(|node| node.capture).collect::<Vec<_>>();
    sorted.sort_by_key(|node| node_order(&node.path));
    let mut seen_buses: Vec<(String, u32)> = Vec::new();
    let mut devices = Vec::new();
    for node in sorted {
        let formats = node_formats(node);
        if formats.is_empty() {
            continue;
        }
        let bus = if node.bus_info.trim().is_empty() {
            // No bus info: the node path is the only location there is.
            key_text(&node.path)
        } else {
            key_text(&node.bus_info)
        };
        let ordinal = if let Some((_, count)) = seen_buses.iter_mut().find(|(seen, _)| *seen == bus)
        {
            *count += 1;
            *count
        } else {
            seen_buses.push((bus.clone(), 1));
            1
        };
        let card = key_text(&node.card);
        let card = if card.is_empty() {
            "camera".to_owned()
        } else {
            card
        };
        let key = V4l2Key {
            bus: bus.clone(),
            ordinal,
            card: card.clone(),
        };
        devices.push(ProbedDevice {
            info: CaptureDevice {
                stable_key: key.text(),
                name: card,
                path: bounded_capture_text(&node.path, MAX_CAPTURE_TEXT_BYTES),
                driver: bounded_capture_text(&node.driver, MAX_CAPTURE_TEXT_BYTES),
                bus,
                audio: audio(node),
                formats,
            },
            no_signal: node.no_signal,
        });
    }
    devices.sort_by(|a, b| a.info.stable_key.cmp(&b.info.stable_key));
    devices
}

/// Sort order of node paths: `/dev/video2` before `/dev/video10`.
fn node_order(path: &str) -> (String, u64) {
    let digits = path.len() - path.bytes().rev().take_while(u8::is_ascii_digit).count();
    (
        path[..digits].to_owned(),
        path[digits..].parse().unwrap_or(u64::MAX),
    )
}

/// The device `key` names among `devices`: the exact stable key; else, for
/// a V4L2 key, the one device with the same card name and ordinal (the
/// device moved to another port); else a device whose node path is `key`.
#[must_use]
pub fn resolve<'a>(key: &str, devices: &'a [ProbedDevice]) -> Option<&'a ProbedDevice> {
    if let Some(device) = devices.iter().find(|device| device.info.stable_key == key) {
        return Some(device);
    }
    if let Some(wanted) = V4l2Key::parse(key) {
        let mut matches = devices.iter().filter(|device| {
            V4l2Key::parse(&device.info.stable_key)
                .is_some_and(|key| key.card == wanted.card && key.ordinal == wanted.ordinal)
        });
        if let (Some(device), None) = (matches.next(), matches.next()) {
            return Some(device);
        }
        return None;
    }
    devices.iter().find(|device| device.info.path == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_and_reject_malformed_text() {
        let first = V4l2Key {
            bus: "usb-0000:00:14.0-1".into(),
            ordinal: 1,
            card: "HD Pro Webcam C920".into(),
        };
        assert_eq!(first.text(), "v4l2:usb-0000:00:14.0-1#HD Pro Webcam C920");
        assert_eq!(V4l2Key::parse(&first.text()), Some(first));
        let second = V4l2Key {
            bus: "PCI:0000:03:00.0".into(),
            ordinal: 2,
            card: "Quad".into(),
        };
        assert_eq!(second.text(), "v4l2:PCI:0000:03:00.0@2#Quad");
        assert_eq!(V4l2Key::parse(&second.text()), Some(second));
        for bad in [
            "",
            "v4l2:",
            "v4l2:#Cam",
            "v4l2:usb-1#",
            "usb-1#Cam",
            "v4l2:usb@1#Cam",
        ] {
            assert_eq!(V4l2Key::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn stepwise_ranges_offer_standard_sizes_and_rates() {
        assert_eq!(
            stepwise_sizes((16, 1920, 2), (16, 1080, 2)),
            [
                (640, 360),
                (640, 480),
                (720, 480),
                (720, 576),
                (960, 540),
                (1024, 576),
                (1280, 720),
                (1600, 900),
                (1920, 1080)
            ]
        );
        assert_eq!(
            stepwise_sizes((1280, 1280, 1), (720, 720, 1)),
            [(1280, 720)]
        );
        let rates = interval_rates(&RawIntervals::Range {
            min: (1, 30),
            max: (1, 1),
        });
        let rates = rates
            .iter()
            .map(|rate| (rate.numerator(), rate.denominator()))
            .collect::<Vec<_>>();
        assert_eq!(
            rates,
            [(24_000, 1_001), (24, 1), (25, 1), (30_000, 1_001), (30, 1)]
        );
        let discrete = interval_rates(&RawIntervals::Discrete(vec![
            (1, 30),
            (1, 30),
            (1_001, 60_000),
            (0, 1),
        ]));
        assert_eq!(
            discrete,
            [
                FrameRate::new(30, 1).unwrap(),
                FrameRate::new(60_000, 1_001).unwrap()
            ]
        );
    }

    #[test]
    fn field_orders_follow_the_standard() {
        assert_eq!(field_scan(field::NONE, 1080), CaptureScan::Progressive);
        assert_eq!(
            field_scan(field::INTERLACED_TB, 1080),
            CaptureScan::InterlacedTff
        );
        assert_eq!(
            field_scan(field::INTERLACED_BT, 576),
            CaptureScan::InterlacedBff
        );
        assert_eq!(
            field_scan(field::INTERLACED, 480),
            CaptureScan::InterlacedBff
        );
        assert_eq!(
            field_scan(field::INTERLACED, 576),
            CaptureScan::InterlacedTff
        );
    }

    #[test]
    fn node_paths_sort_numerically() {
        let mut paths = ["/dev/video10", "/dev/video2", "/dev/video0"];
        paths.sort_by_key(|path| node_order(path));
        assert_eq!(paths, ["/dev/video0", "/dev/video2", "/dev/video10"]);
    }
}
