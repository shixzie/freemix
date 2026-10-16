//! Real V4L2 hardware (or `v4l2loopback`) coverage.
//!
//! Without `/dev/video*` nodes this test does nothing, unless
//! `FM_REQUIRE_V4L2=1` makes their absence a failure. With nodes, every
//! enumerated device must carry a parseable stable key and at least one
//! format; with `FM_REQUIRE_V4L2=1` the first device must also deliver a
//! picture through the `ffmpeg -f v4l2` acquisition within ten seconds
//! (a `v4l2loopback` node needs a producer writing to it).

#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use fm_io_linux::monitor::CaptureProvider;
use fm_io_linux::{
    CaptureSupervisor, FfmpegAcquirer, PulledPicture, SupervisorTiming, V4l2Key, V4l2Provider,
};
use fm_types::FrameRate;
use fm_types::capture::{CaptureSettings, CaptureTarget};

fn required() -> bool {
    std::env::var("FM_REQUIRE_V4L2").as_deref() == Ok("1")
}

#[test]
fn real_nodes_enumerate_and_deliver_when_required() {
    let probe = fm_io_linux::v4l2::SystemV4l2;
    let nodes = probe.node_paths();
    if nodes.is_empty() {
        assert!(
            !required(),
            "FM_REQUIRE_V4L2=1 but there is no /dev/video* node"
        );
        return;
    }
    let mut provider = V4l2Provider::system();
    let devices = provider.enumerate().expect("V4L2 enumeration");
    for device in &devices {
        assert!(
            V4l2Key::parse(&device.info.stable_key).is_some(),
            "{}",
            device.info.stable_key
        );
        assert!(!device.info.formats.is_empty(), "{:?}", device.info);
        eprintln!(
            "V4L2 device {} at {} ({} formats, audio {:?})",
            device.info.stable_key,
            device.info.path,
            device.info.formats.len(),
            device.info.audio
        );
    }
    if !required() {
        return;
    }
    let device = devices
        .first()
        .expect("FM_REQUIRE_V4L2=1 needs a capture device");
    let acquirer = FfmpegAcquirer::v4l2();
    let mut supervisor = CaptureSupervisor::new(
        device.info.stable_key.clone(),
        CaptureSettings {
            audio: fm_types::capture::CaptureAudio::None,
            ..CaptureSettings::default()
        },
        CaptureTarget {
            width: 640,
            height: 360,
            rate: FrameRate::new(30, 1).unwrap(),
        },
        SupervisorTiming::default(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut pictures = 0;
    while pictures == 0 && Instant::now() < deadline {
        let now = Instant::now();
        supervisor.update(&acquirer, &devices, now);
        if matches!(supervisor.pull(now).picture, PulledPicture::New(_)) {
            pictures += 1;
        }
        std::thread::sleep(Duration::from_millis(33));
    }
    assert!(pictures > 0, "{:?}", supervisor.status());
}
