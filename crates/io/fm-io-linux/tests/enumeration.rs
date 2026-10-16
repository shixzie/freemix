//! V4L2 enumeration from fixture probes: capture-node filtering, format
//! parsing (YUYV/NV12/MJPEG, sizes, rates, interlaced field order), stable
//! keys, ALSA pairing, hot-plug diffs, and device resolution.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use fm_io_linux::monitor::{CaptureProvider, HotplugEvent, V4l2Probe, V4l2Provider, diff};
use fm_io_linux::probe::{RawFormat, RawIntervals, RawNode, RawSize, devices_from_nodes, field};
use fm_io_linux::sysfs::{FixtureSysfs, paired_alsa_device};
use fm_io_linux::{DeviceSnapshot, ProbedDevice, V4l2Key, resolve};
use fm_types::FrameRate;
use fm_types::capture::{
    CaptureFormat, CapturePixelFormat, CaptureScan, CaptureTarget, MAX_CAPTURE_DEVICE_FORMATS,
    negotiate,
};

fn size(width: u32, height: u32, intervals: &[(u32, u32)]) -> RawSize {
    RawSize {
        width,
        height,
        intervals: RawIntervals::Discrete(intervals.to_vec()),
    }
}

/// A UVC webcam: YUYV up to 1080p at 5 fps, MJPEG up to 1080p30, and an
/// H.264 format the runtime does not decode.
fn webcam(path: &str, bus: &str) -> RawNode {
    RawNode {
        path: path.into(),
        driver: "uvcvideo".into(),
        card: "HD Pro Webcam C920".into(),
        bus_info: bus.into(),
        capture: true,
        formats: vec![
            RawFormat {
                fourcc: *b"YUYV",
                sizes: vec![
                    size(640, 480, &[(1, 30), (1, 15)]),
                    size(1280, 720, &[(1, 10)]),
                    size(1920, 1080, &[(1, 5)]),
                ],
            },
            RawFormat {
                fourcc: *b"MJPG",
                sizes: vec![
                    size(1280, 720, &[(1, 30)]),
                    size(1920, 1080, &[(1, 30), (1, 24)]),
                ],
            },
            RawFormat {
                fourcc: *b"H264",
                sizes: vec![size(1920, 1080, &[(1, 30)])],
            },
        ],
        current: Some((640, 480, field::NONE)),
        no_signal: false,
    }
}

/// The metadata node every UVC camera adds: same bus, no video capture.
fn metadata(path: &str, bus: &str) -> RawNode {
    RawNode {
        capture: false,
        formats: Vec::new(),
        current: None,
        ..webcam(path, bus)
    }
}

/// An HDMI/SDI capture card locked to 1080i59.94, top field first.
fn capture_card(no_signal: bool) -> RawNode {
    RawNode {
        path: "/dev/video4".into(),
        driver: "magewell".into(),
        card: "Pro Capture HDMI".into(),
        bus_info: "PCI:0000:03:00.0".into(),
        capture: true,
        formats: vec![
            RawFormat {
                fourcc: *b"YUYV",
                sizes: vec![size(1920, 1080, &[(1_001, 30_000)])],
            },
            RawFormat {
                fourcc: *b"NV12",
                sizes: vec![size(1920, 1080, &[(1_001, 30_000)])],
            },
        ],
        current: Some((1920, 1080, field::INTERLACED_TB)),
        no_signal,
    }
}

/// A vivid-style stepwise device with a continuous interval range.
fn stepwise() -> RawNode {
    RawNode {
        path: "/dev/video6".into(),
        driver: "vivid".into(),
        card: "vivid".into(),
        bus_info: "platform:vivid-000".into(),
        capture: true,
        formats: vec![RawFormat {
            fourcc: *b"NV12",
            sizes: fm_io_linux::probe::stepwise_sizes((16, 3840, 2), (16, 2160, 2))
                .into_iter()
                .map(|(width, height)| RawSize {
                    width,
                    height,
                    intervals: RawIntervals::Range {
                        min: (1, 60),
                        max: (1, 1),
                    },
                })
                .collect(),
        }],
        current: Some((640, 360, field::NONE)),
        no_signal: false,
    }
}

fn rate(numerator: u32, denominator: u32) -> FrameRate {
    FrameRate::new(numerator, denominator).unwrap()
}

#[test]
fn capture_nodes_parse_into_devices_with_formats_and_stable_keys() {
    let nodes = [
        metadata("/dev/video1", "usb-0000:00:14.0-1"),
        webcam("/dev/video0", "usb-0000:00:14.0-1"),
        capture_card(false),
        stepwise(),
    ];
    let devices = devices_from_nodes(&nodes, |_| None);
    let keys = devices
        .iter()
        .map(|device| device.info.stable_key.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            "v4l2:PCI:0000:03:00.0#Pro Capture HDMI",
            "v4l2:platform:vivid-000#vivid",
            "v4l2:usb-0000:00:14.0-1#HD Pro Webcam C920",
        ]
    );

    let webcam = &devices[2].info;
    assert_eq!(webcam.path, "/dev/video0");
    assert_eq!(webcam.driver, "uvcvideo");
    assert_eq!(webcam.bus, "usb-0000:00:14.0-1");
    let tokens = webcam
        .formats
        .iter()
        .map(CaptureFormat::token)
        .collect::<Vec<_>>();
    assert_eq!(
        tokens,
        [
            "1920x1080@5/1:yuyv:p",
            "1280x720@10/1:yuyv:p",
            "640x480@30/1:yuyv:p",
            "640x480@15/1:yuyv:p",
            "1920x1080@30/1:mjpeg:p",
            "1920x1080@24/1:mjpeg:p",
            "1280x720@30/1:mjpeg:p",
        ],
        "H.264 is skipped; pixel format, then size and rate order"
    );

    let card = &devices[0].info;
    assert_eq!(
        card.formats
            .iter()
            .map(|format| (format.label(), format.scan))
            .collect::<Vec<_>>(),
        [
            (
                "1920x1080 59.94i YUYV".to_owned(),
                CaptureScan::InterlacedTff
            ),
            (
                "1920x1080 59.94i NV12".to_owned(),
                CaptureScan::InterlacedTff
            ),
        ]
    );

    let vivid = &devices[1].info;
    assert!(vivid.formats.len() <= MAX_CAPTURE_DEVICE_FORMATS);
    for (width, height) in [(720, 480), (1280, 720), (1920, 1080), (3840, 2160)] {
        for fps in [
            rate(24_000, 1_001),
            rate(25, 1),
            rate(30_000, 1_001),
            rate(50, 1),
            rate(60_000, 1_001),
            rate(60, 1),
        ] {
            assert!(
                vivid.formats.iter().any(|format| format.width == width
                    && format.height == height
                    && format.rate == fps
                    && format.pixel_format == CapturePixelFormat::Nv12),
                "{width}x{height} at {fps:?}"
            );
        }
    }
    // SD, HD, and UHD progressive and interlaced rates are covered.
    let classes = devices
        .iter()
        .flat_map(|device| device.info.formats.iter())
        .map(|format| (format.class(), format.scan.is_interlaced()))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(classes.len() >= 4, "{classes:?}");
}

#[test]
fn a_card_with_two_capture_nodes_on_one_bus_numbers_the_second() {
    let mut second = capture_card(false);
    second.path = "/dev/video5".into();
    let devices = devices_from_nodes(&[capture_card(false), second], |_| None);
    assert_eq!(
        devices[0].info.stable_key,
        "v4l2:PCI:0000:03:00.0#Pro Capture HDMI"
    );
    assert_eq!(
        devices[1].info.stable_key,
        "v4l2:PCI:0000:03:00.0@2#Pro Capture HDMI"
    );
    assert_eq!(
        V4l2Key::parse(&devices[1].info.stable_key).map(|key| key.ordinal),
        Some(2)
    );
}

#[test]
fn nodes_without_bus_info_or_usable_formats_are_handled() {
    let mut loopback = webcam("/dev/video9", "");
    loopback.card = "Loop#back@1".into();
    let mut h264_only = webcam("/dev/video3", "usb-2");
    h264_only.formats.retain(|format| format.fourcc == *b"H264");
    let devices = devices_from_nodes(&[loopback, h264_only], |_| None);
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].info.stable_key, "v4l2:/dev/video9#Loop_back_1");
    assert_eq!(
        resolve("/dev/video9", &devices).map(|device| &device.info.name),
        Some(&"Loop_back_1".to_owned())
    );
}

fn sysfs_fixture() -> FixtureSysfs {
    let usb = "/sys/devices/pci0000:00/0000:00:14.0/usb1/1-4";
    let pci = "/sys/devices/pci0000:00/0000:00:1c.0/0000:03:00.0";
    let mut sysfs = FixtureSysfs::default();
    sysfs.links.insert(
        "/sys/class/video4linux/video0/device".into(),
        format!("{usb}/1-4:1.0"),
    );
    sysfs.links.insert(
        "/sys/class/video4linux/video4/device".into(),
        pci.to_owned(),
    );
    sysfs.links.insert(
        "/sys/class/video4linux/video6/device".into(),
        "/sys/devices/platform/vivid.0".into(),
    );
    sysfs.dirs.insert(
        "/sys/class/sound".into(),
        vec![
            "card0".into(),
            "card1".into(),
            "card2".into(),
            "controlC0".into(),
        ],
    );
    // card0: the motherboard codec.
    sysfs.links.insert(
        "/sys/class/sound/card0/device".into(),
        "/sys/devices/pci0000:00/0000:00:1f.3".into(),
    );
    sysfs.dirs.insert(
        "/sys/class/sound/card0".into(),
        vec!["pcmC0D0c".into(), "pcmC0D0p".into()],
    );
    // card1: the webcam microphone, another interface of the same USB
    // device.
    sysfs.links.insert(
        "/sys/class/sound/card1/device".into(),
        format!("{usb}/1-4:1.2"),
    );
    sysfs.dirs.insert(
        "/sys/class/sound/card1".into(),
        vec!["id".into(), "pcmC1D0c".into()],
    );
    sysfs
        .files
        .insert("/sys/class/sound/card1/id".into(), "C920\n".into());
    // card2: the capture card's embedded audio on the same PCI function,
    // capture PCM 1.
    sysfs
        .links
        .insert("/sys/class/sound/card2/device".into(), pci.to_owned());
    sysfs.dirs.insert(
        "/sys/class/sound/card2".into(),
        vec!["pcmC2D1c".into(), "pcmC2D3c".into(), "pcmC2D0p".into()],
    );
    sysfs
}

#[test]
fn audio_devices_pair_by_usb_device_or_pci_function() {
    let sysfs = sysfs_fixture();
    assert_eq!(
        paired_alsa_device(&sysfs, "/dev/video0").as_deref(),
        Some("hw:CARD=C920,DEV=0")
    );
    assert_eq!(
        paired_alsa_device(&sysfs, "/dev/video4").as_deref(),
        Some("hw:CARD=2,DEV=1")
    );
    assert_eq!(paired_alsa_device(&sysfs, "/dev/video6"), None);
    assert_eq!(paired_alsa_device(&sysfs, "/dev/video8"), None);
}

/// A probe over a mutable fixture node table.
#[derive(Clone, Default)]
struct FixtureProbe {
    nodes: Arc<Mutex<BTreeMap<String, RawNode>>>,
}

impl FixtureProbe {
    fn set(&self, nodes: Vec<RawNode>) {
        *self.nodes.lock().unwrap() = nodes
            .into_iter()
            .map(|node| (node.path.clone(), node))
            .collect();
    }
}

impl V4l2Probe for FixtureProbe {
    fn node_paths(&self) -> Vec<String> {
        self.nodes.lock().unwrap().keys().cloned().collect()
    }

    fn probe(&self, path: &str) -> Result<RawNode, String> {
        self.nodes
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| format!("{path}: No such device"))
    }
}

#[test]
fn the_provider_reports_hotplug_renumbering_and_format_changes() {
    let probe = FixtureProbe::default();
    let mut provider = V4l2Provider::new(probe.clone(), sysfs_fixture());
    assert_eq!(provider.backend(), "v4l2");
    assert_eq!(provider.enumerate(), Ok(Vec::new()));

    probe.set(vec![
        webcam("/dev/video0", "usb-0000:00:14.0-1"),
        metadata("/dev/video1", "usb-0000:00:14.0-1"),
    ]);
    let plugged = provider.enumerate().unwrap();
    assert_eq!(plugged.len(), 1);
    assert_eq!(plugged[0].info.audio.as_deref(), Some("hw:CARD=C920,DEV=0"));
    let events = diff(&[], &plugged);
    assert!(matches!(&events[..], [HotplugEvent::Added(device)] if device.path == "/dev/video0"));

    // Unplugged.
    probe.set(Vec::new());
    let unplugged = provider.enumerate().unwrap();
    assert!(matches!(
        &diff(&plugged, &unplugged)[..],
        [HotplugEvent::Removed(_)]
    ));

    // Replugged into the same port, enumerated at another node: same key.
    probe.set(vec![webcam("/dev/video2", "usb-0000:00:14.0-1")]);
    let replugged = provider.enumerate().unwrap();
    assert_eq!(replugged[0].info.stable_key, plugged[0].info.stable_key);
    assert_eq!(replugged[0].info.path, "/dev/video2");
    assert!(matches!(
        &diff(&plugged, &replugged)[..],
        [HotplugEvent::Changed { .. }]
    ));

    // Moved to another port: a new key, resolved by card name.
    probe.set(vec![webcam("/dev/video0", "usb-0000:00:14.0-3")]);
    let moved = provider.enumerate().unwrap();
    assert_ne!(moved[0].info.stable_key, plugged[0].info.stable_key);
    assert_eq!(
        resolve(&plugged[0].info.stable_key, &moved).map(|device| device.info.path.as_str()),
        Some("/dev/video0")
    );
    // Two cameras of that name: ambiguous, so not resolved.
    probe.set(vec![
        webcam("/dev/video0", "usb-0000:00:14.0-3"),
        webcam("/dev/video2", "usb-0000:00:14.0-4"),
    ]);
    let two = provider.enumerate().unwrap();
    assert!(resolve(&plugged[0].info.stable_key, &two).is_none());

    // A capture card's source changes from 1080i to 720p: a changed device
    // whose negotiated format follows.
    probe.set(vec![capture_card(false)]);
    let before = provider.enumerate().unwrap();
    let mut card = capture_card(false);
    card.formats = vec![RawFormat {
        fourcc: *b"YUYV",
        sizes: vec![size(1280, 720, &[(1, 60)])],
    }];
    card.current = Some((1280, 720, field::NONE));
    probe.set(vec![card]);
    let after = provider.enumerate().unwrap();
    assert!(matches!(
        &diff(&before, &after)[..],
        [HotplugEvent::Changed { .. }]
    ));
    let program = CaptureTarget {
        width: 1920,
        height: 1080,
        rate: rate(30_000, 1_001),
    };
    assert_eq!(
        negotiate(&before[0].info.formats, None, program).map(|format| format.label()),
        Some("1920x1080 59.94i YUYV".to_owned())
    );
    assert_eq!(
        negotiate(&after[0].info.formats, None, program).map(|format| format.label()),
        Some("1280x720 60p YUYV".to_owned())
    );

    // No signal is reported per device.
    probe.set(vec![capture_card(true)]);
    let lost = provider.enumerate().unwrap();
    assert!(lost[0].no_signal);
}

#[test]
fn snapshots_become_bounded_inventories() {
    let devices = devices_from_nodes(&[webcam("/dev/video0", "usb-1"), stepwise()], |_| None);
    let snapshot = DeviceSnapshot {
        generation: 3,
        devices,
        error: Some("x".repeat(400)),
    };
    let inventory = snapshot.inventory("v4l2");
    assert_eq!(inventory.backend.as_deref(), Some("v4l2"));
    assert_eq!(inventory.devices.len(), 2);
    assert!(inventory.error.unwrap().len() <= 160);
    let _: &[ProbedDevice] = &snapshot.devices;
}
