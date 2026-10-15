//! OR-011: two independent fullscreen display outputs in a native daemon.
//!
//! The daemon presents two display outputs to the diagnostic display
//! inventory host: each output is placed with its own source and scaling
//! into an off-screen raster sized to its attached display, through the same
//! GPU placement transform a real borderless window uses. The test then
//! unplugs and re-plugs one display (once under a new platform id) by
//! rewriting the inventory file and checks that only that output waits and
//! that it reattaches while the other keeps presenting.
//!
//! A second test drives the same host over the wire (Protocol 2.42): the
//! project's display-output slots are edited live with
//! `set_display_output` / `clear_display_output` (including a Preview
//! source composited for the display), undone, and saved, while the
//! presenter's per-slot state and inventory arrive as runtime events and
//! answer `display_inventory_request`.
//!
//! The tests need a native GPU adapter (Metal, Vulkan, or Direct3D 12; a
//! software Vulkan driver such as lavapipe suffices). Without one the daemon
//! refuses to start and the test is skipped, unless
//! `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

#![cfg(all(feature = "native-media", unix))]

use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use fm_model::{
    AudioBus, Input, InputKind, Layer, LayerGeometry, MainMix, Output, Project, ProjectSettings,
    Rgba8, Rotation, Scene, SimulatedAudio, SimulatedInput, SimulatedVideo, SolidColor, SourceRef,
    StartupPolicy,
};
use fm_persistence::{
    ProjectPosition, ProjectStore, RuntimeFadeToBlack, RuntimeManualTransitions,
    RuntimeOverlayChannel, RuntimeOverlays, RuntimeRouting, StoredProject,
};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    DisplayInventoryRequest, DisplayOutputConfig, DisplayOutputSlot, EventPayload,
    HandshakeRequest, Role, RuntimeLifecycleEvent, SnapshotMessage, WaitReason,
    WireDisplayOutputState, WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_TIMEOUT: Duration = Duration::from_secs(30);
const RED: [u8; 3] = [200, 30, 30];
const BLUE: [u8; 3] = [30, 60, 200];
const GREEN: [u8; 3] = [30, 180, 60];
const PROGRAM_OUTPUT: u128 = 9_101;
const CLEAN_OUTPUT: u128 = 9_102;

fn id(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(id(value))
}

/// A 64x36 show with red input 1 on Program and two project outputs. A
/// full-frame overlay of blue input 2 is on air and included only in output
/// 9101 (Program, the first output), so output 9102 is a red clean feed.
#[allow(clippy::too_many_lines)]
fn save_project(path: &Path) {
    let rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(id(9_100)),
        "Native display outputs",
        ProjectSettings {
            frame_rate: rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(64, 36).unwrap(),
                frame_rate: rate,
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
    let bus = BusId::new(id(9_150));
    project.add_audio_bus(AudioBus {
        id: bus,
        name: "Program bus".into(),
        sends: Vec::new(),
    });
    for (index, color) in [(1, RED), (2, BLUE)] {
        project.add_input(Input {
            id: input(index),
            name: format!("Solid {index}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(color[0], color[1], color[2], 255)),
                SimulatedAudio::Silence,
            )),
            required_capabilities: Vec::new(),
        });
        let scene = SceneId::new(id(9_110 + index));
        project.add_scene(Scene {
            id: scene,
            name: format!("Scene {index}"),
            background: Rgba8::OPAQUE_BLACK,
            layers: vec![Layer {
                id: fm_model::LayerId::new(1).unwrap(),
                name: "Source".into(),
                source: SourceRef::Input(input(index)),
                enabled: true,
                geometry: LayerGeometry::new(0, 0, 64, 36, Rotation::Deg0),
                crop: None,
                mask: None,
                key: None,
                effects: Vec::new(),
                opacity: u8::MAX,
                z_order: 0,
            }],
            animation: fm_model::SceneAnimation::default(),
        });
        project.add_output(Output {
            id: OutputId::new(id(if index == 1 {
                PROGRAM_OUTPUT
            } else {
                CLEAN_OUTPUT
            })),
            name: format!("Output {index}"),
            video_source: scene,
            audio_source: bus,
            startup: StartupPolicy::ReconcileDesiredState,
            required_capabilities: Vec::new(),
        });
    }
    // A third, green input only Preview selects.
    project.add_input(Input {
        id: input(3),
        name: "Solid 3".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(GREEN[0], GREEN[1], GREEN[2], 255)),
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(input(1), input(2)));
    let mut overlays = RuntimeOverlays::default();
    let channel = RuntimeOverlayChannel {
        source: Some(input(2)),
        active: true,
        included_outputs: vec![OutputId::new(id(PROGRAM_OUTPUT))],
        ..RuntimeOverlayChannel::empty()
    };
    overlays.desired[0] = channel.clone();
    overlays.realized[0] = channel;
    let stored = StoredProject::from_project_with_complete_runtime_state(
        project,
        RuntimeRouting {
            desired_program_id: Some(input(1)),
            realized_program_id: Some(input(1)),
            desired_preview_id: Some(input(2)),
            realized_preview_id: Some(input(2)),
        },
        RuntimeManualTransitions::default(),
        RuntimeFadeToBlack::default(),
        overlays,
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    ProjectStore::new(path).unwrap().save(&stored).unwrap();
}

/// Atomically replaces the diagnostic display inventory.
fn write_inventory(path: &Path, lines: &[&str]) {
    let staging = path.with_extension("staging");
    fs::write(&staging, lines.join("\n")).unwrap();
    fs::rename(staging, path).unwrap();
}

const LEFT: &str = "id=left\tname=Left\tx=0\ty=0\tmode=32x32@60";
const RIGHT: &str = "id=right\tname=Right\tx=32\ty=0\tmode=32x32@60";
const RIGHT_RENUMBERED: &str = "id=right-2\tname=Right\tx=32\ty=0\tmode=32x32@60";

struct Daemon {
    child: Child,
    stderr: Receiver<String>,
    address: Option<SocketAddr>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(project: &Path, inventory: &Path) -> Result<Daemon, String> {
    start_with(
        project,
        inventory,
        &[
            "--display-output",
            "display=name:Left,source=program,scaling=fit",
            "--display-output",
            &format!("display=id:right,source=output:{CLEAN_OUTPUT},scaling=fill,hdr=auto"),
        ],
    )
}

fn start_with(project: &Path, inventory: &Path, options: &[&str]) -> Result<Daemon, String> {
    let inventory = inventory.to_str().unwrap();
    let mut arguments = options.to_vec();
    arguments.extend(["--diagnostic-display-inventory", inventory]);
    spawn(project, &arguments)
}

fn spawn(project: &Path, options: &[&str]) -> Result<Daemon, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("serve")
        .arg(project)
        .arg("--native-media")
        .args(options)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot spawn native daemon: {error}"))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (ready_sender, ready) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = ready_sender.send(line);
    });
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let line = ready.recv_timeout(STARTUP_TIMEOUT).unwrap_or_default();
    let mut daemon = Daemon {
        child,
        stderr: receiver,
        address: None,
    };
    if let Ok(record) = line.parse::<ReadinessRecord>() {
        daemon.address = Some(record.address);
        return Ok(daemon);
    }
    let _ = daemon.child.kill();
    let diagnostics = daemon.stderr.try_iter().collect::<Vec<_>>().join("\n");
    Err(format!("native daemon did not become ready: {diagnostics}"))
}

fn fields(line: &str) -> BTreeMap<&str, &str> {
    line.split('\t')
        .filter_map(|field| field.split_once('='))
        .collect()
}

impl Daemon {
    /// Waits for a record with `prefix` whose fields include every `expected` pair.
    fn expect(&self, prefix: &str, expected: &[(&str, &str)]) -> String {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        let mut seen = Vec::new();
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            let Ok(line) = self.stderr.recv_timeout(remaining) else {
                break;
            };
            if line.starts_with(prefix) {
                let values = fields(&line);
                if expected
                    .iter()
                    .all(|(key, value)| values.get(key) == Some(value))
                {
                    return line;
                }
            }
            seen.push(line);
        }
        panic!(
            "no {prefix} record with {expected:?} within {EVENT_TIMEOUT:?}; saw:\n{}",
            seen.join("\n")
        );
    }

    fn terminate(mut self) -> Vec<String> {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let mut lines = Vec::new();
        while let Ok(line) = self.stderr.recv_timeout(Duration::from_secs(2)) {
            lines.push(line);
        }
        lines
    }
}

fn rgba(value: &str) -> [u8; 4] {
    let parts = value
        .split(',')
        .map(|part| part.parse::<u8>().unwrap())
        .collect::<Vec<_>>();
    [parts[0], parts[1], parts[2], parts[3]]
}

fn assert_close(actual: [u8; 4], expected: [u8; 3], context: &str) {
    assert!(
        actual[..3]
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.abs_diff(expected) <= 3)
            && actual[3] == 255,
        "{context}: {actual:?} is not close to {expected:?}"
    );
}

#[test]
fn two_display_outputs_present_independent_sources_and_recover_from_hot_plug() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("display-outputs.freemix");
    save_project(&project);
    let inventory = directory.path().join("displays.tsv");
    write_inventory(&inventory, &[LEFT, RIGHT]);

    let daemon = match start(&project, &inventory) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native display output integration skipped: {error}");
            return;
        }
    };

    // Both outputs attach to their chosen displays with their own source,
    // scaling, and HDR report; neither has a border or a cursor.
    let left = daemon.expect(
        "FREEMIXD_DISPLAY_OUTPUT\t",
        &[("output", "1"), ("state", "active"), ("display", "left")],
    );
    let left = fields(&left);
    assert_eq!(left["source"], "program");
    assert_eq!(left["scaling"], "fit");
    assert_eq!(left["mode"], "32x32@60000");
    assert_eq!(left["hdr"], "sdr");
    assert_eq!(left["borderless"], "true");
    assert_eq!(left["cursor"], "hidden");
    let right = daemon.expect(
        "FREEMIXD_DISPLAY_OUTPUT\t",
        &[("output", "2"), ("state", "active"), ("display", "right")],
    );
    let right = fields(&right);
    assert_eq!(right["source"], format!("output:{CLEAN_OUTPUT}"));
    assert_eq!(right["scaling"], "fill");
    assert_eq!(right["hdr"], "sdr-capability-unknown");

    // Output 1 fits Program (blue overlay included) into a square display:
    // black bars above and below. Output 2 fills the square with the red
    // clean feed of output 9102: no bars.
    let frame = daemon.expect("FREEMIXD_DISPLAY_FRAME\t", &[("output", "1")]);
    let frame = fields(&frame);
    assert_eq!((frame["width"], frame["height"]), ("32", "32"));
    assert_close(rgba(frame["center"]), BLUE, "fitted Program center");
    assert_close(rgba(frame["corner"]), [0, 0, 0], "fitted Program bar");
    let frame = daemon.expect("FREEMIXD_DISPLAY_FRAME\t", &[("output", "2")]);
    let frame = fields(&frame);
    assert_close(rgba(frame["center"]), RED, "filled clean-feed center");
    assert_close(rgba(frame["corner"]), RED, "filled clean-feed corner");

    // Unplugging the right display parks only output 2.
    write_inventory(&inventory, &[LEFT]);
    daemon.expect(
        "FREEMIXD_DISPLAY_OUTPUT\t",
        &[
            ("output", "2"),
            ("state", "waiting"),
            ("reason", "display-absent"),
        ],
    );
    let before = daemon.expect("FREEMIXD_DISPLAY_FRAME\t", &[("output", "1")]);
    let after = daemon.expect("FREEMIXD_DISPLAY_FRAME\t", &[("output", "1")]);
    let presented = |line: &str| fields(line)["presented"].parse::<u64>().unwrap();
    assert!(
        presented(&after) > presented(&before),
        "output 1 kept presenting"
    );

    // The display returns under its original id and output 2 reattaches.
    write_inventory(&inventory, &[LEFT, RIGHT]);
    daemon.expect(
        "FREEMIXD_DISPLAY_OUTPUT\t",
        &[("output", "2"), ("state", "active"), ("display", "right")],
    );
    let frame = daemon.expect(
        "FREEMIXD_DISPLAY_FRAME\t",
        &[("output", "2"), ("display", "right")],
    );
    assert_close(rgba(fields(&frame)["center"]), RED, "reattached output");

    // Unplugged again, it returns renumbered: it is recovered by name.
    write_inventory(&inventory, &[LEFT]);
    daemon.expect(
        "FREEMIXD_DISPLAY_OUTPUT\t",
        &[("output", "2"), ("state", "waiting")],
    );
    write_inventory(&inventory, &[LEFT, RIGHT_RENUMBERED]);
    daemon.expect(
        "FREEMIXD_DISPLAY_OUTPUT\t",
        &[("output", "2"), ("state", "active"), ("display", "right-2")],
    );
    daemon.expect(
        "FREEMIXD_DISPLAY_FRAME\t",
        &[("output", "2"), ("display", "right-2")],
    );

    let tail = daemon.terminate();
    let totals = tail
        .iter()
        .find(|line| line.starts_with("FREEMIXD_DISPLAY_OUTPUTS\t"))
        .unwrap_or_else(|| panic!("no display totals in:\n{}", tail.join("\n")));
    let totals = fields(totals);
    assert!(totals["frames_presented"].parse::<u64>().unwrap() > 0);
    assert_eq!(totals["failures"], "0");
}

#[test]
fn display_outputs_refuse_unknown_sources_before_readiness() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("display-outputs.freemix");
    save_project(&project);
    let output = Command::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("serve")
        .arg(&project)
        .arg("--native-media")
        .args([
            "--display-output",
            "display=0,source=output:4242",
            "--diagnostic-display-inventory",
        ])
        .arg(directory.path().join("displays.tsv"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("output 4242 does not exist"), "{stderr}");
    assert!(output.stdout.is_empty(), "no readiness was advertised");
}

/// A real borderless, cursor-hidden window on the first display of the
/// running window system (for example Xvfb with a software Vulkan driver).
/// Skipped without a window system or GPU unless `FM_REQUIRE_DISPLAY_SURFACE=1`.
#[cfg(feature = "display-surface")]
#[test]
fn a_real_display_output_window_attaches_and_presents() {
    let required = std::env::var("FM_REQUIRE_DISPLAY_SURFACE").as_deref() == Ok("1");
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        assert!(
            !required,
            "FM_REQUIRE_DISPLAY_SURFACE=1 without a window system"
        );
        eprintln!("display surface integration skipped: no window system");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("display-window.freemix");
    save_project(&project);
    let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("serve")
        .arg(&project)
        .arg("--native-media")
        .args([
            "--display-output",
            &format!("display=index:0,source=output:{CLEAN_OUTPUT},scaling=fill"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    thread::spawn(move || {
        let mut sink = String::new();
        let _ = BufReader::new(stdout).read_line(&mut sink);
    });
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let daemon = Daemon {
        child,
        stderr: receiver,
        address: None,
    };
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut seen = Vec::new();
    let mut attached = None;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Ok(line) = daemon.stderr.recv_timeout(remaining) else {
            break;
        };
        if line.starts_with("FREEMIXD_DISPLAY_OUTPUT\t") && fields(&line)["state"] == "active" {
            attached = Some(line);
            break;
        }
        seen.push(line);
    }
    let Some(attached) = attached else {
        assert!(
            !required,
            "no display output attached:\n{}",
            seen.join("\n")
        );
        eprintln!(
            "display surface integration skipped: no window or GPU:\n{}",
            seen.join("\n")
        );
        return;
    };
    let attached = fields(&attached);
    assert_eq!(attached["output"], "1");
    assert_eq!(attached["borderless"], "true");
    assert_eq!(attached["cursor"], "hidden");
    thread::sleep(Duration::from_secs(2));
    let tail = daemon.terminate();
    let totals = tail
        .iter()
        .find(|line| line.starts_with("FREEMIXD_DISPLAY_OUTPUTS\t"))
        .unwrap_or_else(|| panic!("no display totals in:\n{}", tail.join("\n")));
    assert!(
        fields(totals)["frames_presented"].parse::<u64>().unwrap() > 0,
        "{}",
        tail.join("\n")
    );
}

/// A raw Protocol 2.42 client that keeps every message it has not matched
/// yet, so expectations may be checked in any order.
struct WireClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    unmatched: Vec<WireMessage>,
    next_command: u64,
}

impl WireClient {
    fn connect(address: SocketAddr) -> (Self, SnapshotMessage) {
        let stream = TcpStream::connect_timeout(&address, STARTUP_TIMEOUT).unwrap();
        stream.set_nodelay(true).unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            unmatched: Vec::new(),
            next_command: 0,
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-display-outputs-test".into(),
            client_type: ClientType::Studio,
            desired_role: Role::Admin,
            resume_cursor: None,
        }));
        let snapshot = client.expect("snapshot", |message| match message {
            WireMessage::Snapshot(snapshot) => Some(snapshot.clone()),
            _ => None,
        });
        (client, snapshot)
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    /// Waits for the first message (kept or new) that `matches` accepts.
    fn expect<T>(
        &mut self,
        expected: &str,
        mut matches: impl FnMut(&WireMessage) -> Option<T>,
    ) -> T {
        if let Some(position) = self
            .unmatched
            .iter()
            .position(|message| matches(message).is_some())
        {
            let message = self.unmatched.remove(position);
            return matches(&message).unwrap();
        }
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("timed out waiting for {expected}"));
            self.reader
                .get_mut()
                .set_read_timeout(Some(remaining))
                .unwrap();
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .unwrap_or_else(|error| panic!("waiting for {expected}: {error}"));
            assert!(read > 0, "daemon closed while waiting for {expected}");
            let message = decode_line(&line).unwrap();
            if let WireMessage::Error(error) = &message {
                panic!("protocol error while waiting for {expected}: {error:?}");
            }
            if let Some(value) = matches(&message) {
                return value;
            }
            if self.unmatched.len() > 4_096 {
                self.unmatched.remove(0);
            }
            self.unmatched.push(message);
        }
    }

    /// Submits one command and waits for its acceptance.
    fn command(&mut self, payload: CommandPayload) -> u64 {
        self.next_command += 1;
        let id = format!("display-{}", self.next_command);
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id.clone(),
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        self.expect(&format!("result of {id}"), |message| match message {
            WireMessage::CommandResult(CommandResult::Accepted {
                id: result,
                revision,
                ..
            }) if *result == id => Some(*revision),
            WireMessage::CommandResult(CommandResult::Rejected {
                id: result,
                code,
                message,
                ..
            }) if *result == id => panic!("{id} was rejected ({code}): {message}"),
            _ => None,
        })
    }

    /// Waits for slot `slot`'s runtime state to satisfy `matches`.
    fn expect_status(
        &mut self,
        slot: DisplayOutputSlot,
        mut matches: impl FnMut(&WireDisplayOutputState) -> bool,
    ) -> WireDisplayOutputState {
        self.expect(
            &format!("display output {slot} status"),
            |message| match message {
                WireMessage::RuntimeEvent(event) => match &event.event {
                    RuntimeLifecycleEvent::DisplayOutputStatus(status)
                        if status.slot == slot && matches(&status.state) =>
                    {
                        Some(status.state.clone())
                    }
                    _ => None,
                },
                _ => None,
            },
        )
    }

    fn expect_changed(&mut self, slot: DisplayOutputSlot) -> Option<DisplayOutputConfig> {
        self.expect("display_output_changed", |message| match message {
            WireMessage::Event(event) => match &event.payload {
                EventPayload::DisplayOutputChanged {
                    slot: changed,
                    config,
                } if *changed == slot => Some(config.as_deref().cloned()),
                _ => None,
            },
            _ => None,
        })
    }
}

fn active_on(state: &WireDisplayOutputState, display: &str) -> bool {
    matches!(state, WireDisplayOutputState::Active(active) if active.id.as_str() == display)
}

#[test]
#[allow(clippy::too_many_lines)] // one live scenario from inventory to save
fn display_output_slots_are_live_project_edits_over_the_wire() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("display-wire.freemix");
    save_project(&project);
    let inventory = directory.path().join("displays.tsv");
    write_inventory(&inventory, &[LEFT, RIGHT]);

    // No --display-output: the project's (empty) slots drive the host.
    let daemon = match start_with(&project, &inventory, &[]) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native display output wire integration skipped: {error}");
            return;
        }
    };
    let (mut client, snapshot) = WireClient::connect(daemon.address.unwrap());
    assert_eq!(snapshot.display_outputs, Vec::new());
    assert_eq!(snapshot.display_output_states, Vec::new());

    // The inventory answers a request once the host has enumerated.
    let deadline = Instant::now() + EVENT_TIMEOUT;
    let listed = loop {
        client.send(&WireMessage::DisplayInventoryRequest(
            DisplayInventoryRequest {
                request_id: "inventory".into(),
            },
        ));
        let reply = client.expect("display_inventory", |message| match message {
            WireMessage::DisplayInventory(reply) if reply.request_id == "inventory" => {
                Some(reply.inventory.clone())
            }
            _ => None,
        });
        if reply.displays.len() == 2 || Instant::now() > deadline {
            break reply;
        }
        thread::sleep(Duration::from_millis(100));
    };
    assert!(listed.presenter);
    assert_eq!(listed.error, None);
    let ids = listed
        .displays
        .iter()
        .map(|display| display.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["left", "right"]);
    assert_eq!(listed.displays[0].name, "Left");
    assert_eq!(listed.displays[0].current_mode.to_string(), "32x32@60000");

    // Slot 1 shows Preview (blue input 2) stretched on the left display.
    let one = DisplayOutputSlot::new(1).unwrap();
    let preview: DisplayOutputConfig = "display=id:left,source=preview,scaling=stretch"
        .parse()
        .unwrap();
    client.command(CommandPayload::SetDisplayOutput {
        slot: one,
        config: Box::new(preview.clone()),
    });
    assert_eq!(client.expect_changed(one), Some(preview.clone()));
    let active = client.expect_status(one, |state| active_on(state, "left"));
    let WireDisplayOutputState::Active(active) = active else {
        unreachable!()
    };
    assert_eq!(active.name, "Left");
    assert!(!active.mode_fallback);
    let frame = daemon.expect(
        "FREEMIXD_DISPLAY_FRAME\t",
        &[("output", "1"), ("source", "preview")],
    );
    assert_close(rgba(fields(&frame)["center"]), BLUE, "Preview of input 2");
    assert_close(rgba(fields(&frame)["corner"]), BLUE, "stretched Preview");

    // Preview follows the switcher and carries none of Program's overlays
    // (output 9101 shows input 2 full frame over Program).
    client.command(CommandPayload::SelectPreview {
        input: WireInputId::from_domain(input(3)),
    });
    let deadline = Instant::now() + EVENT_TIMEOUT;
    loop {
        let frame = daemon.expect(
            "FREEMIXD_DISPLAY_FRAME\t",
            &[("output", "1"), ("source", "preview")],
        );
        let center = rgba(fields(&frame)["center"]);
        if center[..3]
            .iter()
            .zip(GREEN)
            .all(|(actual, expected)| actual.abs_diff(expected) <= 3)
        {
            break;
        }
        assert!(Instant::now() < deadline, "Preview never showed input 3");
    }

    // Unplugging the left display parks the slot and updates the inventory.
    write_inventory(&inventory, &[RIGHT]);
    client.expect_status(one, |state| {
        *state == WireDisplayOutputState::Waiting(WaitReason::DisplayAbsent)
    });
    client.expect("display_inventory event", |message| match message {
        WireMessage::RuntimeEvent(event) => match &event.event {
            RuntimeLifecycleEvent::DisplayInventory(inventory) if inventory.displays.len() == 1 => {
                Some(())
            }
            _ => None,
        },
        _ => None,
    });

    // Moving the slot to the right display rebinds it live.
    let program: DisplayOutputConfig = "display=id:right,source=program".parse().unwrap();
    client.command(CommandPayload::SetDisplayOutput {
        slot: one,
        config: Box::new(program.clone()),
    });
    assert_eq!(client.expect_changed(one), Some(program.clone()));
    client.expect_status(one, |state| active_on(state, "right"));
    daemon.expect(
        "FREEMIXD_DISPLAY_FRAME\t",
        &[("output", "1"), ("display", "right"), ("source", "program")],
    );

    // Clearing the slot releases its display; undo restores it.
    client.command(CommandPayload::ClearDisplayOutput { slot: one });
    assert_eq!(client.expect_changed(one), None);
    daemon.expect(
        "FREEMIXD_DISPLAY_OUTPUT\t",
        &[("output", "1"), ("state", "cleared")],
    );
    client.command(CommandPayload::Undo {
        expected_edit: None,
    });
    assert_eq!(client.expect_changed(one), Some(program.clone()));
    client.expect_status(one, |state| active_on(state, "right"));

    // A reconnecting client sees the slot and its state in the snapshot.
    let (_second, snapshot) = WireClient::connect(daemon.address.unwrap());
    assert_eq!(snapshot.display_outputs.len(), 1);
    assert_eq!(snapshot.display_outputs[0].config, program);
    assert!(active_on(&snapshot.display_output_states[0].state, "right"));
    drop(client);

    // The slot is project configuration: the shutdown checkpoint saves it.
    let tail = daemon.terminate();
    assert!(
        tail.iter()
            .any(|line| line.starts_with("FREEMIXD_DISPLAY_OUTPUTS\t")),
        "{}",
        tail.join("\n")
    );
    let saved = ProjectStore::new(&project).unwrap().load().unwrap();
    assert_eq!(saved.project().display_output(one), Some(&program));
}

/// `--display-presenter` opens the project's slots in real windows and
/// follows live slot edits (for example under Xvfb with a software Vulkan
/// driver). Skipped without a window system or GPU unless
/// `FM_REQUIRE_DISPLAY_SURFACE=1`.
#[cfg(feature = "display-surface")]
#[test]
fn a_real_presenter_window_follows_live_slot_edits() {
    let required = std::env::var("FM_REQUIRE_DISPLAY_SURFACE").as_deref() == Ok("1");
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        assert!(
            !required,
            "FM_REQUIRE_DISPLAY_SURFACE=1 without a window system"
        );
        eprintln!("display presenter integration skipped: no window system");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("display-presenter.freemix");
    save_project(&project);
    let daemon = match spawn(&project, &["--display-presenter"]) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(!required, "{error}");
            eprintln!("display presenter integration skipped: {error}");
            return;
        }
    };
    let (mut client, snapshot) = WireClient::connect(daemon.address.unwrap());
    assert_eq!(snapshot.display_outputs, Vec::new());
    let one = DisplayOutputSlot::new(1).unwrap();
    client.command(CommandPayload::SetDisplayOutput {
        slot: one,
        config: Box::new(
            format!("display=index:0,source=output:{CLEAN_OUTPUT},scaling=fill")
                .parse()
                .unwrap(),
        ),
    });
    let state = client.expect_status(one, |state| {
        matches!(state, WireDisplayOutputState::Active(_))
    });
    let WireDisplayOutputState::Active(active) = state else {
        unreachable!()
    };
    daemon.expect(
        "FREEMIXD_DISPLAY_OUTPUT\t",
        &[
            ("output", "1"),
            ("state", "active"),
            ("display", active.id.as_str()),
        ],
    );
    client.send(&WireMessage::DisplayInventoryRequest(
        DisplayInventoryRequest {
            request_id: "windows".into(),
        },
    ));
    let inventory = client.expect("display_inventory", |message| match message {
        WireMessage::DisplayInventory(reply) if reply.request_id == "windows" => {
            Some(reply.inventory.clone())
        }
        _ => None,
    });
    assert!(inventory.presenter);
    assert!(
        inventory
            .displays
            .iter()
            .any(|display| display.id == active.id)
    );
    thread::sleep(Duration::from_secs(1));
    client.command(CommandPayload::ClearDisplayOutput { slot: one });
    daemon.expect(
        "FREEMIXD_DISPLAY_OUTPUT\t",
        &[("output", "1"), ("state", "cleared")],
    );
    drop(client);
    let tail = daemon.terminate();
    let totals = tail
        .iter()
        .find(|line| line.starts_with("FREEMIXD_DISPLAY_OUTPUTS\t"))
        .unwrap_or_else(|| panic!("no display totals in:\n{}", tail.join("\n")));
    assert!(
        fields(totals)["frames_presented"].parse::<u64>().unwrap() > 0,
        "{}",
        tail.join("\n")
    );
}
