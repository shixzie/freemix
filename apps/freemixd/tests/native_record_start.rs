//! A runtime `RecordStart` must never stall the native daemon loop.
//!
//! The recorder start spawns `FFmpeg` and waits for it to connect; that wait
//! used to run on the daemon loop, freezing frame realization, monitor frames
//! and every session for seconds. These tests hold `FFmpeg`'s start open behind
//! a gate the test controls, so the start cannot finish before the test has
//! observed the loop still working — the ordering is decided by the test, not
//! by a wall-clock threshold.

#![cfg(all(feature = "native-media", unix))]

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
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
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    HandshakeRequest, MonitorFrameAssembler, Role, WireMessage, WireRecorderSlot, decode_line,
    encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
/// Monitor frames the daemon must publish while the start is still pending.
/// `FFmpeg` cannot connect before the test opens the gate, so the daemon has
/// the recorder's whole connect budget to publish them.
const FRAMES_DURING_START: usize = 3;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

/// A 64x48 generator show whose recorder slot is configured but not desired,
/// so no segment opens until a runtime `RecordStart`.
fn save_generator_project(path: &Path) {
    let rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(7_101).unwrap()),
        "Native record start test",
        ProjectSettings {
            frame_rate: rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(64, 48).unwrap(),
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
    for (id, video) in [
        (input(1), SimulatedVideo::Bars),
        (
            input(2),
            SimulatedVideo::Solid(SolidColor::new(24, 80, 160, 255)),
        ),
    ] {
        project.add_input(Input {
            id,
            name: format!("Generator {id}"),
            kind: InputKind::Simulated(SimulatedInput::new(video, SimulatedAudio::Silence)),
            required_capabilities: Vec::new(),
        });
    }
    project.set_main_mix(MainMix::new(input(1), input(2)));
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(input(1)),
            realized_program_id: Some(input(1)),
            desired_preview_id: Some(input(2)),
            realized_preview_id: Some(input(2)),
        },
        ProjectPosition::default(),
        Vec::new(),
    )
    .unwrap();
    ProjectStore::new(path).unwrap().save(&stored).unwrap();
}

/// Installs an `ffmpeg` that never connects to the recorder until `gate`
/// exists, then exits without producing output.
fn install_gated_ffmpeg(bin: &Path, gate: &Path) {
    fs::create_dir_all(bin).unwrap();
    let script = bin.join("ffmpeg");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nwhile [ ! -e '{}' ]; do sleep 0.01; done\nexit 7\n",
            gate.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
}

struct DaemonProcess {
    child: Child,
    address: SocketAddr,
    stderr: mpsc::Receiver<String>,
}

impl DaemonProcess {
    /// Starts a headless native daemon, or returns `None` when this host has
    /// no native media runtime (unless `FM_REQUIRE_NATIVE_MEDIA=1`).
    fn start(project: &Path, output: &Path, bin: &Path) -> Option<Self> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut search = vec![bin.to_path_buf()];
        search.extend(std::env::split_paths(&path));
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .arg("--native-media")
            .arg("--record-program")
            .arg(output)
            .env("PATH", std::env::join_paths(search).unwrap())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = drain(child.stderr.take().unwrap());
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (ready_sender, ready) = mpsc::channel();
        thread::spawn(move || {
            let mut line = String::new();
            let _ = stdout.read_line(&mut line);
            let _ = ready_sender.send(line);
            // Keep draining so the daemon never blocks on a full pipe.
            let _ = io_sink(&mut stdout);
        });
        let line = ready.recv_timeout(PROCESS_TIMEOUT).unwrap_or_default();
        match line.trim_end().parse::<ReadinessRecord>() {
            Ok(readiness) => Some(Self {
                child,
                address: readiness.address,
                stderr,
            }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let detail: String = stderr.try_iter().collect();
                let message = format!("native daemon unavailable: {error}; stderr={detail}");
                assert!(
                    std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                    "FM_REQUIRE_NATIVE_MEDIA=1: {message}"
                );
                eprintln!("native record start test skipped: {message}");
                None
            }
        }
    }

    /// Requests a cooperative stop and returns everything written to stderr.
    fn stop(mut self) -> String {
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                let stderr: String = self.stderr.iter().collect();
                assert!(status.success(), "daemon failed: {stderr}");
                return stderr;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let stderr: String = self.stderr.try_iter().collect();
                panic!("daemon did not stop: {stderr}");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn io_sink(reader: &mut impl Read) -> std::io::Result<u64> {
    std::io::copy(reader, &mut std::io::sink())
}

fn drain(mut reader: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0
                || sender
                    .send(String::from_utf8_lossy(&buffer[..read]).into_owned())
                    .is_err()
            {
                break;
            }
        }
    });
    receiver
}

struct StudioClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl StudioClient {
    fn connect(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream.set_read_timeout(Some(PROCESS_TIMEOUT)).unwrap();
        Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
        }
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
    }

    fn receive(&mut self) -> WireMessage {
        let mut line = String::new();
        assert_ne!(
            self.reader.read_line(&mut line).unwrap(),
            0,
            "daemon closed the session"
        );
        decode_line(&line).unwrap()
    }

    fn handshake(&mut self) {
        self.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-record-start-test".into(),
            client_type: ClientType::Studio,
            desired_role: Role::Operator,
            resume_cursor: None,
        }));
        while !matches!(self.receive(), WireMessage::Snapshot(_)) {}
    }
}

/// While a runtime `RecordStart` is still waiting for `FFmpeg`, the daemon keeps
/// realizing frames and publishing fresh monitor frames, and the start's
/// acknowledgement still arrives only after realization settles and before
/// the events the command published.
#[test]
fn runtime_record_start_keeps_the_daemon_loop_serving_monitor_frames() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("record-start.freemix");
    save_generator_project(&project);
    let bin = directory.path().join("bin");
    let gate = directory.path().join("ffmpeg-gate");
    install_gated_ffmpeg(&bin, &gate);
    let output: PathBuf = directory.path().join("program.mp4");
    let Some(daemon) = DaemonProcess::start(&project, &output, &bin) else {
        return;
    };

    let mut studio = StudioClient::connect(daemon.address);
    studio.handshake();
    let mut assembler = MonitorFrameAssembler::new();
    // Monitor demand is established once the first frame arrives.
    let mut last_frame = loop {
        if let WireMessage::MonitorFrame(part) = studio.receive()
            && let Some(image) = assembler.push(&part).unwrap()
        {
            break image.frame;
        }
    };

    studio.send(&WireMessage::Command(CommandMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        id: "record-start".into(),
        idempotency_key: "record-start-key".into(),
        expected_revision: None,
        deadline_ms: None,
        payload: CommandPayload::RecordStart {
            recorder: WireRecorderSlot::new(1).unwrap(),
        },
    }));

    // FFmpeg cannot connect until the gate opens, so the recorder start is
    // still pending throughout: every frame counted here was realized and
    // published by a loop that kept running while the start was in flight.
    let mut frames_during_start = 0;
    while frames_during_start < FRAMES_DURING_START {
        match studio.receive() {
            WireMessage::MonitorFrame(part) => {
                if let Some(image) = assembler.push(&part).unwrap() {
                    assert!(image.frame > last_frame, "monitor frames must be fresh");
                    last_frame = image.frame;
                    frames_during_start += 1;
                }
            }
            WireMessage::CommandResult(result) => panic!(
                "RecordStart was answered after only {frames_during_start} monitor frames while \
                 FFmpeg had not started: the daemon loop stalled ({result:?})"
            ),
            WireMessage::Event(event) => {
                panic!("an event preceded the RecordStart acknowledgement: {event:?}")
            }
            _ => {}
        }
    }

    // Let FFmpeg fail its start. The acknowledgement is released only now,
    // still ahead of the command's events.
    fs::write(&gate, b"").unwrap();
    let result = loop {
        match studio.receive() {
            WireMessage::CommandResult(result) => break result,
            WireMessage::Event(event) => {
                panic!("an event preceded the RecordStart acknowledgement: {event:?}")
            }
            _ => {}
        }
    };
    assert!(
        matches!(&result, CommandResult::Accepted { id, .. } if id == "record-start"),
        "{result:?}"
    );
    drop(studio);

    let stderr = daemon.stop();
    let failure = stderr
        .lines()
        .find(|line| line.starts_with("FREEMIXD_RECORDER_FAILURE\t"))
        .unwrap_or_else(|| panic!("missing recorder failure notice: {stderr}"));
    assert!(
        failure.contains("\tslot=1\t") && failure.contains("\tfailure=start:"),
        "{failure}"
    );
    assert!(
        !stderr
            .lines()
            .any(|line| line.starts_with("FREEMIXD_RECORDER\t")),
        "a segment that never opened has no recorder report: {stderr}"
    );
    assert!(!output.exists(), "a failed start leaves no segment behind");
}
