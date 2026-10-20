#![cfg(all(feature = "native-media", target_os = "linux"))]

//! Process-level IN-011 on the native runtime: a native daemon whose
//! `DISPLAY` is a private `Xvfb` server captures an application window
//! (followed by its class and a title rule) onto Program through
//! `ffmpeg -f x11grab`. Its state reaches clients as
//! `screen_capture_status` runtime events: live with the window's size;
//! `target_gone` with the black slate on Program when the window closes;
//! live again, on the new window, when an application window matching the
//! rule opens. `screen_capture_targets_request` lists the display's screen
//! and windows.
//!
//! The test needs `FFmpeg`, `Xvfb`, and a native GPU adapter (a software
//! Vulkan driver such as lavapipe suffices). Without them it is skipped,
//! unless `FM_REQUIRE_FFMPEG=1` or `FM_REQUIRE_NATIVE_MEDIA=1` makes that a
//! failure.

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
    CURRENT_PROTOCOL_VERSION, ClientType, HandshakeRequest, HeartbeatMessage,
    MonitorFrameAssembler, MonitorImage, MonitorTap, Role, RuntimeLifecycleEvent,
    ScreenCaptureConfig, ScreenCaptureState, ScreenCaptureStatus, ScreenCaptureTarget,
    ScreenCaptureTargetsRequest, ServerIdentity, TitleMatch, WindowRule, WireInputId, WireMessage,
    decode_line, encode_line,
};
use fm_types::network_stream::StallSlate;
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(40);
const HEARTBEAT_EVERY: Duration = Duration::from_secs(1);
const CLASS: &str = "FreemixDeck";

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn slides() -> WireInputId {
    WireInputId::from_domain(input(2))
}

fn tools_available() -> bool {
    let available = ["ffmpeg", "Xvfb"].iter().all(|tool| {
        Command::new(tool)
            .arg(if *tool == "Xvfb" { "-help" } else { "-version" })
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    });
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG=1 but ffmpeg or Xvfb is unavailable"
    );
    available
}

/// A private `Xvfb` display, killed on drop.
struct Xvfb {
    child: Child,
    display: String,
}

impl Xvfb {
    fn start() -> Self {
        for attempt in 0..40_u32 {
            let number = 200 + (std::process::id() + attempt * 11 + 3) % 600;
            if Path::new(&format!("/tmp/.X11-unix/X{number}")).exists() {
                continue;
            }
            let display = format!(":{number}");
            let mut child = Command::new("Xvfb")
                .args([
                    display.as_str(),
                    "-screen",
                    "0",
                    "640x360x24",
                    "-noreset",
                    "-nolisten",
                    "tcp",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("Xvfb starts");
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                if x11rb::connect(Some(&display)).is_ok() {
                    return Self { child, display };
                }
                thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        panic!("no Xvfb display could start");
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An application window of class [`CLASS`] titled `title`, filled with
/// `rgb`.
struct TestWindow {
    connection: RustConnection,
    window: u32,
}

impl TestWindow {
    fn open(display: &str, title: &str, rgb: u32) -> Self {
        let (connection, screen) = x11rb::connect(Some(display)).unwrap();
        let root = connection.setup().roots[screen].root;
        let window = connection.generate_id().unwrap();
        connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                root,
                40,
                30,
                320,
                180,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new().background_pixel(rgb),
            )
            .unwrap();
        let class = format!("{}\0{CLASS}\0", CLASS.to_lowercase());
        connection
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                class.as_bytes(),
            )
            .unwrap();
        connection
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            )
            .unwrap();
        connection.map_window(window).unwrap();
        connection.flush().unwrap();
        connection.sync().unwrap();
        Self { connection, window }
    }

    fn close(self) {
        self.connection.destroy_window(self.window).unwrap();
        self.connection.flush().unwrap();
        self.connection.sync().unwrap();
    }
}

/// Input 1 is a solid generator on Preview; input 2 captures the
/// application window whose title contains "Slides" on Program, black
/// while it is gone.
fn save_project(path: &Path) {
    let store = ProjectStore::new(path).unwrap();
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(66).unwrap()),
        "Native screen capture",
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
    let config = ScreenCaptureConfig {
        target: ScreenCaptureTarget::Window(WindowRule {
            app: CLASS.into(),
            title: "slides".into(),
            title_match: TitleMatch::Contains,
            window_id: None,
        }),
        slate: StallSlate::Black,
        cursor: false,
        fps: 25,
        ..ScreenCaptureConfig::default()
    };
    project.add_input(Input {
        id: input(2),
        name: "Slides".into(),
        kind: InputKind::ScreenCapture(config),
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
        if thread::panicking() {
            while let Ok(line) = self.log.recv_timeout(Duration::from_secs(2)) {
                eprintln!("daemon: {line}");
            }
        }
    }
}

fn start_native(project: &Path, display: &str) -> Result<NativeDaemon, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("serve")
        .arg(project)
        .arg("--native-media")
        .env("DISPLAY", display)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("XDG_SESSION_TYPE")
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
    assembler: MonitorFrameAssembler,
    server: Option<ServerIdentity>,
    heartbeats: u64,
    last_heartbeat: Instant,
}

impl Client {
    fn connect(address: SocketAddr) -> (Self, fm_protocol::SnapshotMessage) {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            assembler: MonitorFrameAssembler::new(),
            server: None,
            heartbeats: 0,
            last_heartbeat: Instant::now(),
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-screen-capture-test".into(),
            client_type: ClientType::Studio,
            desired_role: Role::Admin,
            resume_cursor: None,
        }));
        let WireMessage::HandshakeResponse(response) = client.receive() else {
            panic!("expected the handshake response");
        };
        client.server = Some(response.server);
        let WireMessage::Snapshot(snapshot) = client.receive() else {
            panic!("expected the snapshot");
        };
        (client, snapshot)
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    fn receive(&mut self) -> WireMessage {
        if let Some(server) = self.server.clone()
            && self.last_heartbeat.elapsed() >= HEARTBEAT_EVERY
        {
            self.heartbeats += 1;
            self.send(&WireMessage::Heartbeat(HeartbeatMessage {
                server,
                sequence: self.heartbeats,
                sent_at_ms: self.heartbeats,
                last_applied: None,
            }));
            self.last_heartbeat = Instant::now();
        }
        let mut line = String::new();
        assert_ne!(
            self.reader.read_line(&mut line).unwrap(),
            0,
            "daemon closed"
        );
        let message = decode_line(&line).unwrap();
        assert!(
            !matches!(&message, WireMessage::Error(_)),
            "daemon error: {message:?}"
        );
        message
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

    /// Waits for a status of the slides input that satisfies `wanted`.
    fn status(
        &mut self,
        what: &str,
        wanted: impl Fn(&ScreenCaptureStatus) -> bool,
    ) -> ScreenCaptureStatus {
        self.wait_for(what, |_, message| {
            let WireMessage::RuntimeEvent(event) = message else {
                return None;
            };
            let RuntimeLifecycleEvent::ScreenCaptureStatus { input, status } = event.event else {
                return None;
            };
            (input == slides() && wanted(&status)).then_some(*status)
        })
    }

    /// Waits until the centre of Program satisfies `wanted`.
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
}

fn red(pixel: [u8; 4]) -> bool {
    pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60
}

fn blue(pixel: [u8; 4]) -> bool {
    pixel[2] > 200 && pixel[0] < 60 && pixel[1] < 60
}

fn black(pixel: [u8; 4]) -> bool {
    pixel[0] < 24 && pixel[1] < 24 && pixel[2] < 24
}

#[test]
fn native_daemon_captures_a_window_and_follows_it_through_closure() {
    if !tools_available() {
        eprintln!("native screen capture integration skipped: ffmpeg or Xvfb unavailable");
        return;
    }
    let xvfb = Xvfb::start();
    let window = TestWindow::open(&xvfb.display, "Quarterly Slides", 0x00ff_0000);
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-screen-capture.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path, &xvfb.display) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native screen capture integration skipped: {error}");
            return;
        }
    };
    let (mut client, snapshot) = Client::connect(daemon.address);
    let [entry] = snapshot.screen_capture_inputs.as_slice() else {
        panic!(
            "one screen capture input: {:?}",
            snapshot.screen_capture_inputs
        );
    };
    assert_eq!(entry.input, slides());

    // The window is found by its class and title and shown on Program.
    let live = if entry.status.state == ScreenCaptureState::Live {
        entry.status.clone()
    } else {
        client.status("live", |status| status.state == ScreenCaptureState::Live)
    };
    assert_eq!((live.width, live.height), (320, 180), "{live:?}");
    assert_eq!(live.window_id, Some(u64::from(window.window)));
    client.shows("the red window on Program", red);

    // The server lists its screen and the window.
    client.send(&WireMessage::ScreenCaptureTargetsRequest(
        ScreenCaptureTargetsRequest {
            request_id: "targets-1".into(),
        },
    ));
    let inventory = client.wait_for("the target list", |_, message| match message {
        WireMessage::ScreenCaptureTargets(targets) if targets.request_id == "targets-1" => {
            Some(targets.inventory)
        }
        _ => None,
    });
    assert!(inventory.supported, "{inventory:?}");
    assert_eq!(inventory.backend, "x11");
    assert_eq!(inventory.screens.len(), 1);
    assert_eq!(
        (inventory.screens[0].width, inventory.screens[0].height),
        (640, 360)
    );
    assert!(
        inventory
            .windows
            .iter()
            .any(|listed| listed.app == CLASS && listed.title == "Quarterly Slides"),
        "{inventory:?}"
    );

    // The window closes: the input reports its target gone and Program
    // shows the black slate.
    window.close();
    let gone = client.status("the target gone", |status| {
        status.state == ScreenCaptureState::TargetGone
    });
    assert!(
        gone.last_error
            .as_deref()
            .is_some_and(|error| error.contains(CLASS)),
        "{gone:?}"
    );
    client.shows("the black slate on Program", black);

    // A new window matching the rule opens: capture follows it.
    let reopened = TestWindow::open(&xvfb.display, "Slides (copy)", 0x0000_00ff);
    let back = client.status("live again", |status| {
        status.state == ScreenCaptureState::Live
    });
    assert_eq!(back.window_id, Some(u64::from(reopened.window)));
    assert!(back.reopens >= 1, "{back:?}");
    client.shows("the blue window on Program", blue);
    drop(daemon);
    reopened.close();
}
