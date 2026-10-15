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
//! The test needs a native GPU adapter (Metal, Vulkan, or Direct3D 12; a
//! software Vulkan driver such as lavapipe suffices). Without one the daemon
//! refuses to start and the test is skipped, unless
//! `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

#![cfg(all(feature = "native-media", unix))]

use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader},
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
use fm_types::{
    AudioFormat, BusId, ChannelLayout, ColorMetadata, FrameRate, InputId, OutputId, PixelFormat,
    ProjectId, SampleFormat, SampleRate, ScanMode, SceneId, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_TIMEOUT: Duration = Duration::from_secs(30);
const RED: [u8; 3] = [200, 30, 30];
const BLUE: [u8; 3] = [30, 60, 200];
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
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(project: &Path, inventory: &Path) -> Result<Daemon, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("serve")
        .arg(project)
        .arg("--native-media")
        .args([
            "--display-output",
            "display=name:Left,source=program,scaling=fit",
            "--display-output",
            &format!("display=id:right,source=output:{CLEAN_OUTPUT},scaling=fill,hdr=auto"),
            "--diagnostic-display-inventory",
        ])
        .arg(inventory)
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
    };
    if line.parse::<ReadinessRecord>().is_ok() {
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
    assert!(stderr.contains("source=output:4242"), "{stderr}");
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
