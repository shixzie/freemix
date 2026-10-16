//! The capture state machine with the fake device provider: acquisition,
//! hot-plug, signal loss and recovery under the same input, stalls,
//! no-signal, renegotiation on a device format change, requested formats,
//! closing, and the polling monitor. No hardware is needed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fm_io_linux::fake::parse_inventory;
use fm_io_linux::monitor::CaptureProvider;
use fm_io_linux::{
    CaptureMonitor, CaptureSupervisor, FakeAcquirer, FakeCaptureWorld, FakeDevice, FakeProvider,
    ProbedDevice, PulledPicture, SupervisorTiming,
};
use fm_types::FrameRate;
use fm_types::capture::{
    CaptureAudio, CaptureDevice, CaptureFormat, CaptureLossReason, CaptureLossSlate,
    CaptureSettings, CaptureSignalState, CaptureTarget,
};

const KEY: &str = "v4l2:usb-0000:00:14.0-1#Fake Cam";
const RED: [u8; 4] = [255, 0, 0, 255];

fn format(token: &str) -> CaptureFormat {
    CaptureFormat::from_token(token).unwrap()
}

fn device(path: &str, formats: &[&str]) -> FakeDevice {
    FakeDevice {
        info: CaptureDevice {
            stable_key: KEY.into(),
            name: "Fake Cam".into(),
            path: path.into(),
            driver: "fake".into(),
            bus: "usb-0000:00:14.0-1".into(),
            audio: Some("hw:CARD=Fake,DEV=0".into()),
            formats: formats.iter().map(|token| format(token)).collect(),
        },
        rgba: RED,
        stalled: false,
        no_signal: false,
    }
}

fn target() -> CaptureTarget {
    CaptureTarget {
        width: 8,
        height: 4,
        rate: FrameRate::new(30, 1).unwrap(),
    }
}

fn timing() -> SupervisorTiming {
    SupervisorTiming {
        stall_after: Duration::from_millis(100),
        start_grace: Duration::from_millis(300),
        retry_after: Duration::from_millis(50),
    }
}

struct Rig {
    world: Arc<FakeCaptureWorld>,
    provider: FakeProvider,
    acquirer: FakeAcquirer,
    supervisor: CaptureSupervisor,
    now: Instant,
}

impl Rig {
    fn new(settings: CaptureSettings) -> Self {
        let world = FakeCaptureWorld::new();
        Self {
            provider: FakeProvider::new(Arc::clone(&world)),
            acquirer: FakeAcquirer::new(Arc::clone(&world)),
            world,
            supervisor: CaptureSupervisor::new(KEY.into(), settings, target(), timing()),
            now: Instant::now(),
        }
    }

    fn devices(&mut self) -> Vec<ProbedDevice> {
        self.provider.enumerate().unwrap()
    }

    /// One program frame `step` after the last.
    fn frame(&mut self, step: Duration) -> PulledPicture {
        self.now += step;
        let devices = self.devices();
        self.supervisor.update(&self.acquirer, &devices, self.now);
        self.supervisor.pull(self.now).picture
    }

    fn state(&self) -> (CaptureSignalState, Option<CaptureLossReason>) {
        let status = self.supervisor.status();
        (status.state, status.reason)
    }
}

const FRAME: Duration = Duration::from_millis(33);

#[test]
fn a_device_goes_live_is_lost_on_unplug_and_recovers_at_a_new_node() {
    let mut rig = Rig::new(CaptureSettings {
        loss: CaptureLossSlate::Black,
        ..CaptureSettings::default()
    });
    rig.world.plug(device(
        "/dev/video0",
        &["1280x720@30/1:yuyv:p", "1920x1080@30/1:mjpeg:p"],
    ));
    let PulledPicture::New(picture) = rig.frame(FRAME) else {
        panic!("a live picture");
    };
    assert_eq!(picture.len(), 8 * 4 * 4);
    assert_eq!(&picture[..4], &RED);
    let status = rig.supervisor.status().clone();
    assert_eq!(status.state, CaptureSignalState::Live);
    assert_eq!(status.device.as_deref(), Some("/dev/video0"));
    assert_eq!(
        status.format.map(|format| format.token()),
        Some("1280x720@30/1:yuyv:p".to_owned()),
        "the smallest device picture covering the (tiny) program"
    );
    assert!(!status.audio, "the fake device has no audio stream");
    assert_eq!(rig.world.acquiring(), ["/dev/video0"]);

    // Unplug: the signal is lost at once, the black slate shows once, and
    // the acquisition stops.
    rig.world.unplug(KEY);
    assert_eq!(rig.frame(FRAME), PulledPicture::Black);
    assert_eq!(
        rig.state(),
        (
            CaptureSignalState::SignalLost,
            Some(CaptureLossReason::Unplugged)
        )
    );
    assert_eq!(rig.frame(FRAME), PulledPicture::Keep);
    assert_eq!(rig.world.acquiring(), Vec::<String>::new());
    assert_eq!(rig.supervisor.status().losses, 1);
    assert_eq!(rig.supervisor.status().device, None);

    // Replugged and enumerated at another node: reacquired there.
    rig.world.plug(device(
        "/dev/video2",
        &["1280x720@30/1:yuyv:p", "1920x1080@30/1:mjpeg:p"],
    ));
    assert!(matches!(rig.frame(FRAME), PulledPicture::New(_)));
    let status = rig.supervisor.status();
    assert_eq!(status.state, CaptureSignalState::Live);
    assert_eq!(status.device.as_deref(), Some("/dev/video2"));
    assert_eq!((status.losses, status.recoveries), (1, 1));
    assert_eq!(status.last_error, None);
    assert_eq!(rig.world.acquiring(), ["/dev/video2"]);
    assert_eq!(rig.supervisor.key(), KEY, "the same input follows it");
}

#[test]
fn a_stalled_device_holds_its_picture_and_recovers_when_pictures_return() {
    let mut rig = Rig::new(CaptureSettings::default());
    rig.world
        .plug(device("/dev/video0", &["1280x720@30/1:yuyv:p"]));
    assert!(matches!(rig.frame(FRAME), PulledPicture::New(_)));
    rig.world.update(KEY, |device| device.stalled = true);
    // Within the stall budget: still live, holding.
    assert_eq!(rig.frame(FRAME), PulledPicture::Keep);
    assert_eq!(rig.state().0, CaptureSignalState::Live);
    for _ in 0..3 {
        let _ = rig.frame(FRAME);
    }
    assert_eq!(
        rig.state(),
        (
            CaptureSignalState::SignalLost,
            Some(CaptureLossReason::Stalled)
        )
    );
    // Hold: no black slate.
    assert_eq!(rig.frame(FRAME), PulledPicture::Keep);
    assert!(rig.supervisor.status().last_error.is_some());
    rig.world.update(KEY, |device| device.stalled = false);
    assert!(matches!(rig.frame(FRAME), PulledPicture::New(_)));
    let status = rig.supervisor.status();
    assert_eq!(status.state, CaptureSignalState::Live);
    assert_eq!((status.losses, status.recoveries), (1, 1));
    assert_eq!(rig.world.starts(), 1, "the same acquisition recovered");
}

#[test]
fn a_device_format_change_renegotiates_and_no_signal_is_a_loss() {
    let mut rig = Rig::new(CaptureSettings::default());
    rig.world
        .plug(device("/dev/video4", &["1920x1080@30000/1001:yuyv:tff"]));
    assert!(matches!(rig.frame(FRAME), PulledPicture::New(_)));
    assert_eq!(
        rig.supervisor.status().format.map(|format| format.label()),
        Some("1920x1080 59.94i YUYV".to_owned())
    );
    // The source switches to 720p60.
    rig.world.update(KEY, |device| {
        device.info.formats = vec![format("1280x720@60/1:yuyv:p")];
    });
    assert!(matches!(rig.frame(FRAME), PulledPicture::New(_)));
    let status = rig.supervisor.status();
    assert_eq!(status.renegotiations, 1);
    assert_eq!(
        status.format.map(|format| format.token()),
        Some("1280x720@60/1:yuyv:p".to_owned())
    );
    assert_eq!(rig.world.starts(), 2);
    assert_eq!(status.state, CaptureSignalState::Live);

    // The source is unplugged from the card: no signal.
    rig.world.update(KEY, |device| device.no_signal = true);
    let _ = rig.frame(FRAME);
    assert_eq!(
        rig.state(),
        (
            CaptureSignalState::SignalLost,
            Some(CaptureLossReason::NoSignal)
        )
    );
    rig.world.update(KEY, |device| device.no_signal = false);
    let _ = rig.frame(FRAME);
    let _ = rig.frame(FRAME);
    let status = rig.supervisor.status();
    assert_eq!(status.state, CaptureSignalState::Live);
    assert_eq!((status.losses, status.recoveries), (1, 1));
}

#[test]
fn requested_formats_audio_and_closing_are_honoured() {
    let requested = format("1280x720@30/1:yuyv:p");
    let mut rig = Rig::new(CaptureSettings {
        format: Some(requested),
        audio: CaptureAudio::None,
        loss: CaptureLossSlate::Hold,
    });
    rig.world.plug(device(
        "/dev/video0",
        &["1280x720@30/1:yuyv:p", "1920x1080@30/1:mjpeg:p"],
    ));
    let _ = rig.frame(FRAME);
    assert_eq!(rig.supervisor.status().format, Some(requested));

    // A settings change reopens in the new format.
    rig.supervisor.set_settings(CaptureSettings {
        format: Some(format("1920x1080@30/1:mjpeg:p")),
        ..CaptureSettings::default()
    });
    let _ = rig.frame(FRAME);
    assert_eq!(
        rig.supervisor.status().format.map(|format| format.token()),
        Some("1920x1080@30/1:mjpeg:p".to_owned())
    );
    assert_eq!(rig.world.starts(), 2);

    // Closing stops acquiring; reopening reacquires.
    rig.supervisor.set_closed(true);
    assert_eq!(rig.frame(FRAME), PulledPicture::Keep);
    assert_eq!(rig.state(), (CaptureSignalState::Idle, None));
    assert_eq!(rig.world.acquiring(), Vec::<String>::new());
    rig.supervisor.set_closed(false);
    assert!(matches!(rig.frame(FRAME), PulledPicture::New(_)));
    assert_eq!(rig.state().0, CaptureSignalState::Live);
    assert_eq!(rig.supervisor.status().losses, 0);
}

#[test]
fn an_absent_device_waits_and_a_device_without_formats_fails() {
    let mut rig = Rig::new(CaptureSettings::default());
    assert_eq!(rig.frame(FRAME), PulledPicture::Keep);
    assert_eq!(
        rig.state(),
        (
            CaptureSignalState::SignalLost,
            Some(CaptureLossReason::Unplugged)
        )
    );
    assert_eq!(rig.supervisor.status().losses, 0, "never live, never lost");
    let mut empty = device("/dev/video0", &[]);
    empty.info.formats.clear();
    rig.world.plug(empty);
    let _ = rig.frame(FRAME);
    assert_eq!(rig.state().0, CaptureSignalState::Failed);
    rig.world.update(KEY, |device| {
        device.info.formats = vec![format("640x480@30/1:yuyv:p")];
    });
    assert!(matches!(rig.frame(FRAME), PulledPicture::New(_)));
    assert_eq!(rig.state().0, CaptureSignalState::Live);
}

#[test]
fn a_device_that_never_delivers_is_lost_after_the_start_grace() {
    let mut rig = Rig::new(CaptureSettings::default());
    let mut stalled = device("/dev/video0", &["640x480@30/1:yuyv:p"]);
    stalled.stalled = true;
    rig.world.plug(stalled);
    let _ = rig.frame(FRAME);
    assert_eq!(rig.state().0, CaptureSignalState::Starting);
    let _ = rig.frame(Duration::from_millis(400));
    assert_eq!(
        rig.state(),
        (
            CaptureSignalState::SignalLost,
            Some(CaptureLossReason::Stalled)
        )
    );
}

#[test]
fn the_monitor_publishes_a_generation_per_change_from_an_inventory_file() {
    let dir = std::env::temp_dir().join(format!("fm-io-linux-monitor-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("devices.tsv");
    let line = format!(
        "key={KEY}\tpath=/dev/video0\tformats=1280x720@30/1:yuyv:p\trgba=00ff00ff\taudio=hw:CARD=Fake,DEV=0\n"
    );
    std::fs::write(&file, format!("# fake devices\n\n{line}")).unwrap();
    let parsed = parse_inventory(&line).unwrap();
    assert_eq!(parsed[0].info.name, "Fake Cam");
    assert_eq!(parsed[0].info.bus, "usb-0000:00:14.0-1");
    assert_eq!(parsed[0].rgba, [0, 255, 0, 255]);
    assert!(parse_inventory("key=x\tpath=/dev/video0").is_err());
    assert!(parse_inventory("key=x\tpath=/dev/video0\tformats=bad").is_err());
    assert!(
        parse_inventory("key=x\tpath=/dev/video0\tformats=640x480@30/1:yuyv:p\tstalled=2").is_err()
    );

    let world = FakeCaptureWorld::from_file(file.clone());
    let monitor = CaptureMonitor::spawn(
        Box::new(FakeProvider::new(Arc::clone(&world))),
        Duration::from_millis(10),
    )
    .unwrap();
    assert_eq!(monitor.backend(), "fake");
    let first = monitor.snapshot();
    assert_eq!(first.devices.len(), 1);
    let inventory = first.inventory(monitor.backend());
    assert_eq!(inventory.devices[0].stable_key, KEY);

    std::fs::write(&file, "").unwrap();
    monitor.rescan();
    let deadline = Instant::now() + Duration::from_secs(5);
    while monitor.snapshot().generation == first.generation && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let second = monitor.snapshot();
    assert_eq!(second.generation, first.generation + 1);
    assert_eq!(second.devices, Vec::new());
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        monitor.snapshot().generation,
        second.generation,
        "unchanged"
    );
    drop(monitor);
    let _ = std::fs::remove_dir_all(dir);
}
