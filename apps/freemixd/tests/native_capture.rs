#![cfg(feature = "native-media")]

//! Process-level IN-001 on the native runtime, without capture hardware: a
//! native daemon started with `--diagnostic-capture-inventory <file>` uses
//! the deterministic fake capture backend, whose devices follow the file.
//! The test rewrites the file to plug, unplug, move, and reformat a device
//! and checks over the wire that the capture input on Program goes live and
//! shows the device's picture, loses its signal to the black slate when
//! unplugged, recovers under the same input ID when the device returns at
//! another node, and renegotiates when its formats change. It also asks for
//! the device inventory, edits the input's settings, and adds and removes a
//! capture input live.
//!
//! The test needs a native GPU adapter (a software Vulkan driver such as
//! lavapipe suffices). Without one it is skipped, unless
//! `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use fm_model::{
    Input, InputKind, MainMix, Project, ProjectSettings, SimulatedAudio, SimulatedInput,
    SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CaptureDevicesRequest, ClientType, CommandMessage, CommandPayload,
    CommandResult, EventPayload, HandshakeRequest, LiveInputSource, MonitorFrameAssembler,
    MonitorImage, MonitorTap, Role, RuntimeLifecycleEvent, WireInputId, WireMessage, decode_line,
    encode_line,
};
use fm_types::capture::{
    CaptureInputConfig, CaptureLossReason, CaptureLossSlate, CaptureSettings, CaptureSignalState,
    CaptureStatus,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

#[path = "support/heartbeat.rs"]
mod heartbeat;
use heartbeat::Heartbeat;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(40);
const CAMERA_KEY: &str = "v4l2:usb-0000:00:14.0-1#Fake Cam";
const SPARE_KEY: &str = "v4l2:usb-0000:00:14.0-2#Spare Cam";

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn camera() -> WireInputId {
    WireInputId::from_domain(input(2))
}

/// Rewrites the fake inventory atomically: one device per line.
fn write_inventory(path: &Path, lines: &[String]) {
    let staging = path.with_extension("staging");
    std::fs::write(&staging, lines.join("\n")).unwrap();
    std::fs::rename(staging, path).unwrap();
}

fn device(key: &str, name: &str, node: &str, formats: &str, rgba: &str) -> String {
    format!("key={key}\tname={name}\tpath={node}\tformats={formats}\trgba={rgba}")
}

/// Input 1 is a silent solid generator on Preview, input 2 the capture
/// input on Program, with the black loss slate.
fn save_project(path: &Path) {
    let store = ProjectStore::new(path).unwrap();
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(45).unwrap()),
        "Native capture",
        ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(320, 180).unwrap(),
                frame_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(48_000).unwrap(),
                sample_format: SampleFormat::F32,
                channels: ChannelLayout::stereo(),
            },
        },
    );
    project.add_input(Input {
        id: input(1),
        name: "Solid".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(12, 34, 56, 255)),
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input(2),
        name: "Camera".into(),
        kind: InputKind::Device {
            stable_key: CAMERA_KEY.into(),
            capture: CaptureSettings {
                loss: CaptureLossSlate::Black,
                ..CaptureSettings::default()
            },
        },
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(input(2), input(1)));
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(input(2)),
            realized_program_id: Some(input(2)),
            desired_preview_id: Some(input(1)),
            realized_preview_id: Some(input(1)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    store.save(&stored).unwrap();
}

struct NativeDaemon {
    child: Child,
    address: SocketAddr,
    log: mpsc::Receiver<String>,
}

impl Drop for NativeDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_native(project: &Path, inventory: &Path) -> Result<NativeDaemon, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("serve")
        .arg(project)
        .arg("--native-media")
        .arg("--diagnostic-capture-inventory")
        .arg(inventory)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot spawn native daemon: {error}"))?;
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(line);
    });
    let line = receiver.recv_timeout(STARTUP_TIMEOUT).unwrap_or_default();
    if let Ok(readiness) = line.parse::<ReadinessRecord>() {
        let (log_sender, log) = mpsc::channel();
        if let Some(stderr) = child.stderr.take() {
            thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let _ = log_sender.send(line);
                }
            });
        }
        return Ok(NativeDaemon {
            child,
            address: readiness.address,
            log,
        });
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    Err(format!("native daemon did not become ready: {stderr}"))
}

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    heartbeat: Heartbeat,
    assembler: MonitorFrameAssembler,
    commands: u64,
}

impl Client {
    fn connect(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            heartbeat: Heartbeat::new(),
            assembler: MonitorFrameAssembler::new(),
            commands: 0,
        }
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    fn receive(&mut self) -> WireMessage {
        loop {
            if let Some(beat) = self.heartbeat.due() {
                self.send(&beat);
            }
            let mut line = String::new();
            assert_ne!(
                self.reader.read_line(&mut line).unwrap(),
                0,
                "daemon closed"
            );
            let message = decode_line(&line).unwrap();
            if !self.heartbeat.observe(&message) {
                return message;
            }
        }
    }

    fn wait_for<T>(
        &mut self,
        what: &str,
        mut found: impl FnMut(&mut Self, WireMessage) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            let message = self.receive();
            if let Some(value) = found(self, message) {
                return value;
            }
        }
    }

    fn program(&mut self, message: WireMessage) -> Option<MonitorImage> {
        let WireMessage::MonitorFrame(part) = message else {
            return None;
        };
        let image = self.assembler.push(&part).unwrap()?;
        (image.tap == MonitorTap::Program).then_some(image)
    }

    /// Waits for a capture status of `of` that satisfies `wanted`.
    fn status(
        &mut self,
        of: WireInputId,
        what: &str,
        wanted: impl Fn(&CaptureStatus) -> bool,
    ) -> CaptureStatus {
        self.wait_for(what, |_, message| {
            let WireMessage::RuntimeEvent(event) = message else {
                return None;
            };
            let RuntimeLifecycleEvent::CaptureStatus { input, status } = event.event else {
                return None;
            };
            (input == of && wanted(&status)).then_some(*status)
        })
    }

    fn shows(&mut self, what: &str, wanted: impl Fn([u8; 4]) -> bool) {
        self.wait_for(what, |client, message| {
            let image = client.program(message)?;
            let width = usize::from(image.width);
            let at = (usize::from(image.height) / 2 * width + width / 2) * 4;
            let pixel = [
                image.rgba[at],
                image.rgba[at + 1],
                image.rgba[at + 2],
                image.rgba[at + 3],
            ];
            wanted(pixel).then_some(())
        });
    }

    fn accept(&mut self, payload: CommandPayload) -> Vec<EventPayload> {
        self.commands += 1;
        let id = format!("command-{}", self.commands);
        let described = format!("{payload:?}");
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id,
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        let mut events = Vec::new();
        self.wait_for("the command result", |_, message| match message {
            WireMessage::Event(event) => {
                events.push(event.payload);
                None
            }
            WireMessage::CommandResult(result) => {
                assert!(
                    matches!(result, CommandResult::Accepted { .. }),
                    "{described}: {result:?}"
                );
                Some(())
            }
            _ => None,
        });
        events
    }
}

impl Client {
    /// Accepts `payload` and waits for the event `wanted` finds, whether
    /// it arrives before or after the command result.
    fn accept_event<T>(
        &mut self,
        payload: CommandPayload,
        what: &str,
        wanted: impl Fn(&EventPayload) -> Option<T>,
    ) -> T {
        let mut events = self.accept(payload);
        self.wait_for(what, |_, message| {
            if let WireMessage::Event(event) = message {
                events.push(event.payload);
            }
            events.iter().find_map(&wanted)
        })
    }
}

fn red(pixel: [u8; 4]) -> bool {
    pixel[0] > 160 && pixel[1] < 90 && pixel[2] < 90
}

fn green(pixel: [u8; 4]) -> bool {
    pixel[0] < 90 && pixel[1] > 160 && pixel[2] < 90
}

fn black(pixel: [u8; 4]) -> bool {
    pixel[0] < 30 && pixel[1] < 30 && pixel[2] < 30
}

#[test]
#[allow(clippy::too_many_lines)]
fn native_daemon_captures_a_device_through_loss_recovery_and_renegotiation() {
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-capture.freemix");
    let inventory = directory.path().join("capture-inventory.txt");
    save_project(&project_path);
    let formats = "640x360@25/1:yuyv:p,1280x720@30/1:mjpeg:p";
    write_inventory(
        &inventory,
        &[device(
            CAMERA_KEY,
            "Fake Cam",
            "/dev/video0",
            formats,
            "ff0000ff",
        )],
    );
    let daemon = match start_native(&project_path, &inventory) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native capture integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "native-capture-test".into(),
        client_type: ClientType::Studio,
        desired_role: Role::Admin,
        resume_cursor: None,
    }));
    assert!(matches!(
        client.receive(),
        WireMessage::HandshakeResponse(_)
    ));
    let WireMessage::Snapshot(snapshot) = client.receive() else {
        panic!("expected the snapshot");
    };
    let [entry] = snapshot.capture_inputs.as_slice() else {
        panic!("one capture input: {:?}", snapshot.capture_inputs);
    };
    assert_eq!(entry.input, camera());
    assert_eq!(entry.config.stable_key, CAMERA_KEY);
    assert_eq!(entry.config.settings.loss, CaptureLossSlate::Black);

    // The device is present: the input goes live at the format nearest the
    // program, and Program shows the device's picture.
    let live = client.status(camera(), "live", |status| {
        status.state == CaptureSignalState::Live
    });
    assert_eq!(live.device.as_deref(), Some("/dev/video0"));
    assert_eq!(
        live.format
            .as_ref()
            .map(fm_types::capture::CaptureFormat::token),
        Some("640x360@25/1:yuyv:p".to_owned()),
        "{live:?}"
    );
    client.shows("the device's red picture on Program", red);

    // The inventory answers a request with the fake backend's device.
    client.send(&WireMessage::CaptureDevicesRequest(CaptureDevicesRequest {
        request_id: "devices-1".into(),
    }));
    let devices = client.wait_for("the capture devices", |_, message| match message {
        WireMessage::CaptureDevices(answer) if answer.request_id == "devices-1" => {
            Some(answer.inventory)
        }
        _ => None,
    });
    assert_eq!(devices.backend.as_deref(), Some("fake"));
    assert_eq!(devices.devices.len(), 1, "{devices:?}");
    assert_eq!(devices.devices[0].stable_key, CAMERA_KEY);
    assert_eq!(devices.devices[0].formats.len(), 2);

    // Unplugged: the signal is lost and Program shows the black slate.
    write_inventory(&inventory, &[]);
    let lost = client.status(camera(), "the signal lost", |status| {
        status.state == CaptureSignalState::SignalLost
    });
    assert_eq!(lost.reason, Some(CaptureLossReason::Unplugged), "{lost:?}");
    assert_eq!(lost.losses, 1, "{lost:?}");
    client.shows("the black loss slate on Program", black);

    // Replugged at another node: the same input recovers there.
    write_inventory(
        &inventory,
        &[device(
            CAMERA_KEY,
            "Fake Cam",
            "/dev/video4",
            formats,
            "00ff00ff",
        )],
    );
    let recovered = client.status(camera(), "live again", |status| {
        status.state == CaptureSignalState::Live
    });
    assert_eq!(recovered.device.as_deref(), Some("/dev/video4"));
    assert_eq!(recovered.recoveries, 1, "{recovered:?}");
    client.shows("the replugged device's green picture", green);

    // The device drops its nearest format: the input renegotiates.
    write_inventory(
        &inventory,
        &[device(
            CAMERA_KEY,
            "Fake Cam",
            "/dev/video4",
            "1280x720@30/1:mjpeg:p",
            "00ff00ff",
        )],
    );
    let renegotiated = client.status(camera(), "a renegotiated format", |status| {
        status.state == CaptureSignalState::Live && status.renegotiations == 1
    });
    assert_eq!(
        renegotiated
            .format
            .as_ref()
            .map(fm_types::capture::CaptureFormat::token),
        Some("1280x720@30/1:mjpeg:p".to_owned())
    );

    // A settings edit is accepted and announced.
    let changed = client.accept_event(
        CommandPayload::SetCaptureSettings {
            input: camera(),
            settings: Box::new(CaptureSettings {
                loss: CaptureLossSlate::Hold,
                ..CaptureSettings::default()
            }),
        },
        "a capture_settings_changed event",
        |event| match event {
            EventPayload::CaptureSettingsChanged { input, settings } if *input == camera() => {
                Some(settings.clone())
            }
            _ => None,
        },
    );
    assert_eq!(changed.loss, CaptureLossSlate::Hold);

    // A second device is plugged and added as a capture input live, then
    // removed live.
    write_inventory(
        &inventory,
        &[
            device(
                CAMERA_KEY,
                "Fake Cam",
                "/dev/video4",
                "1280x720@30/1:mjpeg:p",
                "00ff00ff",
            ),
            device(
                SPARE_KEY,
                "Spare Cam",
                "/dev/video6",
                "640x360@25/1:yuyv:p",
                "0000ffff",
            ),
        ],
    );
    let spare = WireInputId::from_domain(input(3));
    client.accept_event(
        CommandPayload::AddInput {
            input: spare,
            name: "Spare".into(),
            source: LiveInputSource::Capture(Box::new(CaptureInputConfig::new(SPARE_KEY))),
        },
        "an input_added event",
        |event| match event {
            EventPayload::InputAdded {
                input,
                capture: Some(config),
                ..
            } if *input == spare && config.stable_key == SPARE_KEY => Some(()),
            _ => None,
        },
    );
    let spare_live = client.status(spare, "the added input live", |status| {
        status.state == CaptureSignalState::Live
    });
    assert_eq!(spare_live.device.as_deref(), Some("/dev/video6"));
    client.accept_event(
        CommandPayload::RemoveInput { input: spare },
        "an input_removed event",
        |event| match event {
            EventPayload::InputRemoved { input } if *input == spare => Some(()),
            _ => None,
        },
    );
    drop(client);

    // The daemon logged the loss.
    let deadline = Instant::now() + WAIT;
    let mut logged = Vec::new();
    while !logged.iter().any(|line: &String| {
        line.starts_with("FREEMIXD_CAPTURE") && line.contains("state=signal_lost")
    }) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match daemon.log.recv_timeout(remaining) {
            Ok(record) => logged.push(record),
            Err(error) => panic!("no signal_lost record ({error}): {logged:?}"),
        }
    }
}
