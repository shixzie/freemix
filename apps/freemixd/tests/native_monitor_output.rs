#![cfg(all(feature = "native-media", unix))]

//! Process-level headphone output (AU-001, Protocol 2.53): a running native
//! daemon plays the monitor bus on the chosen local audio device at the
//! monitor output's own level and mute, while Program keeps its own level.
//!
//! The daemon opens the device through an `FFmpeg` child that reads
//! interleaved `f32le` PCM on its standard input and writes `-f alsa
//! <device>`. The test puts a wrapper named `ffmpeg` first on the daemon's
//! `PATH`: for a device named `phones-*` it records the arguments and keeps
//! the PCM it receives, and for every other invocation it runs the real
//! `FFmpeg`. So the test reads exactly what reached the device, sees a
//! device that does not exist fail through the real `FFmpeg`, and plays the
//! real ALSA `null` device where ALSA is available.
//!
//! The test needs `FFmpeg` and a native GPU adapter (a software Vulkan
//! driver such as lavapipe suffices). Without them it is skipped, unless
//! `FM_REQUIRE_FFMPEG=1` or `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

use std::{
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
    AudioMetersMessage, AudioMixerOperation, AudioMonitorOutputState, CURRENT_PROTOCOL_VERSION,
    ClientType, CommandMessage, CommandPayload, CommandResult, HandshakeRequest, HeartbeatMessage,
    ImportItemSpec, InputLifecycleOperation, MediaOperation, MonitorFrameAssembler, MonitorTap,
    Role, ServerIdentity, WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(40);
/// A Master peak this loud (millionths of full scale) is the clip's tone.
const LOUD: u32 = 50_000;
/// The PCM window the test measures: 50 ms of 48 kHz stereo `f32`.
const WINDOW_BYTES: usize = 2_400 * 2 * 4;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn wire(value: u128) -> WireInputId {
    WireInputId::from_domain(input(value))
}

fn tools_available() -> bool {
    let available = ["ffmpeg", "ffprobe"].iter().all(|tool| {
        Command::new(tool)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG=1 but ffmpeg/ffprobe are unavailable"
    );
    available
}

/// The real `ffmpeg` on `PATH`.
fn real_ffmpeg() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("ffmpeg"))
        .find(|candidate| candidate.is_file())
        .expect("ffmpeg is on PATH")
}

/// Whether the real `FFmpeg` can play the ALSA `null` device here.
fn alsa_null_available() -> bool {
    Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=stereo",
            "-t",
            "0.1",
        ])
        .args(["-f", "alsa", "null"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Writes the `ffmpeg` wrapper into `bin`; captured devices land in
/// `capture`.
fn write_wrapper(bin: &Path, capture: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    let script = format!(
        "#!/bin/sh\n\
         last=''\n\
         for arg in \"$@\"; do last=\"$arg\"; done\n\
         case \"$last\" in\n\
         phones-*) printf '%s\\n' \"$@\" > '{capture}'/\"$last\".args; exec cat > '{capture}'/\"$last\".pcm ;;\n\
         esac\n\
         exec '{real}' \"$@\"\n",
        capture = capture.display(),
        real = real_ffmpeg().display(),
    );
    let path = bin.join("ffmpeg");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A 20 s, 25 fps, 320x180 red clip with a 440 Hz tone.
fn generate_clip(path: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=320x180:r=25:d=20,\
             setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:d=20",
        ])
        .args(["-c:v", "mpeg2video", "-g", "12", "-q:v", "2"])
        .args(["-pix_fmt", "yuv420p"])
        .args(["-color_primaries", "bt709", "-color_trc", "bt709"])
        .args(["-colorspace", "bt709"])
        .args(["-c:a", "pcm_f32le", "-f", "matroska", "-y"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "fixture generation failed: {status}");
}

/// Input 1 is a silent blue generator on Program.
fn save_project(path: &Path) {
    let store = ProjectStore::new(path).unwrap();
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(53).unwrap()),
        "Native monitor output",
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
    for id in [1, 2] {
        project.add_input(Input {
            id: input(id),
            name: format!("Blue {id}"),
            kind: InputKind::Simulated(SimulatedInput::new(
                SimulatedVideo::Solid(SolidColor::new(20, 40, 220, 255)),
                SimulatedAudio::Silence,
            )),
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

fn start_native(project: &Path, bin: &Path) -> Result<NativeDaemon, String> {
    let path = std::env::join_paths(
        std::iter::once(bin.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("serve")
        .arg(project)
        .arg("--native-media")
        .env("PATH", path)
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
    commands: u64,
    /// The daemon's identity, which every heartbeat names.
    server: Option<ServerIdentity>,
    heartbeats: u64,
    last_heartbeat: Instant,
}

impl Client {
    fn connect(address: SocketAddr) -> (Self, fm_protocol::AudioMixerStatus) {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            commands: 0,
            server: None,
            heartbeats: 0,
            last_heartbeat: Instant::now(),
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "native-monitor-output-test".into(),
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
        (client, snapshot.audio_mixer)
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    /// The next message; a heartbeat goes out every second so the session
    /// stays open however long the test waits.
    fn receive(&mut self) -> WireMessage {
        if let Some(server) = self.server.clone()
            && self.last_heartbeat.elapsed() >= Duration::from_secs(1)
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
        decode_line(&line).unwrap()
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

    /// Waits until Program shows the clip's red picture, so it is open.
    fn shows_red_program(&mut self) {
        let mut assembler = MonitorFrameAssembler::new();
        self.wait_for("the clip on Program", |_, message| {
            let WireMessage::MonitorFrame(part) = message else {
                return None;
            };
            let image = assembler.push(&part).unwrap()?;
            if image.tap != MonitorTap::Program {
                return None;
            }
            let width = usize::from(image.width);
            let at = (usize::from(image.height) / 2 * width + width / 2) * 4;
            let pixel = &image.rgba[at..at + 3];
            (pixel[0] > 160 && pixel[1] < 90 && pixel[2] < 90).then_some(())
        });
    }

    /// Waits for a meter sample that satisfies `wanted`.
    fn meters(&mut self, what: &str, wanted: impl Fn(&AudioMetersMessage) -> bool) {
        self.wait_for(what, |_, message| match message {
            WireMessage::AudioMeters(meters) if wanted(&meters) => Some(()),
            _ => None,
        });
    }

    /// Waits until the monitor output reports `state`.
    fn output(&mut self, what: &str, state: AudioMonitorOutputState) {
        self.meters(what, |meters| {
            meters
                .monitor
                .as_ref()
                .is_some_and(|monitor| monitor.output == state)
        });
    }

    fn submit(&mut self, payload: CommandPayload) -> CommandResult {
        self.commands += 1;
        let id = format!("command-{}", self.commands);
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id,
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        self.wait_for("the command result", |_, message| match message {
            WireMessage::CommandResult(result) => Some(result),
            _ => None,
        })
    }

    fn accept(&mut self, payload: CommandPayload) {
        let described = format!("{payload:?}");
        let result = self.submit(payload);
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{described}: {result:?}"
        );
    }

    fn device(&mut self, device: Option<&str>) {
        self.accept(CommandPayload::AudioMixer(
            AudioMixerOperation::SetMonitorDevice {
                device: device.map(str::to_owned),
            },
        ));
    }

    fn level(&mut self, gain_millidb: i32, muted: bool) {
        self.accept(CommandPayload::AudioMixer(
            AudioMixerOperation::SetMonitorLevel {
                gain_millidb,
                muted,
            },
        ));
    }
}

fn master_peak(meters: &AudioMetersMessage) -> u32 {
    meters
        .master
        .iter()
        .map(|channel| channel.peak_millionths)
        .max()
        .unwrap_or(0)
}

/// The peak of the newest [`WINDOW_BYTES`] of PCM the device received, or
/// `None` until that much has arrived.
fn device_peak(pcm: &Path) -> Option<f32> {
    let bytes = std::fs::read(pcm).ok()?;
    let whole = bytes.len() / 8 * 8;
    let window = bytes.get(whole.checked_sub(WINDOW_BYTES)?..whole)?;
    Some(
        window
            .as_chunks::<4>()
            .0
            .iter()
            .map(|sample| f32::from_le_bytes(*sample).abs())
            .fold(0.0, f32::max),
    )
}

/// Waits until the newest PCM on the device satisfies `wanted`, reading
/// the session meanwhile.
fn device_reaches(
    client: &mut Client,
    pcm: &Path,
    what: &str,
    wanted: impl Fn(f32) -> bool,
) -> f32 {
    let deadline = Instant::now() + WAIT;
    let mut checked = Instant::now();
    loop {
        if checked.elapsed() >= Duration::from_millis(50) {
            if let Some(peak) = device_peak(pcm).filter(|peak| wanted(*peak)) {
                return peak;
            }
            checked = Instant::now();
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {:?}",
            device_peak(pcm)
        );
        client.receive();
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn native_daemon_plays_the_monitor_bus_on_a_headphone_device() {
    if !tools_available() {
        eprintln!("native monitor output integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let bin = directory.path().join("bin");
    let capture = directory.path().join("capture");
    std::fs::create_dir_all(&capture).unwrap();
    write_wrapper(&bin, &capture);
    let clip = directory.path().join("clip.mkv");
    generate_clip(&clip);
    let project_path = directory.path().join("native-monitor-output.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path, &bin) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native monitor output integration skipped: {error}");
            return;
        }
    };
    let (mut client, mixer) = Client::connect(daemon.address);
    // A new project is meter-only at unity.
    assert_eq!(mixer.monitor, fm_protocol::AudioMonitorStatus::default());
    client.output("a meter-only monitor bus", AudioMonitorOutputState::Off);

    // The clip's tone plays on Program; the monitor bus follows Master.
    client.accept(CommandPayload::InputLifecycle(
        InputLifecycleOperation::Import {
            items: vec![ImportItemSpec {
                input: wire(11),
                name: Some("Clip".into()),
                uri: clip.to_str().unwrap().to_owned(),
            }],
        },
    ));
    client.accept(CommandPayload::SelectPreview { input: wire(11) });
    client.accept(CommandPayload::Cut);
    client.shows_red_program();
    client.accept(CommandPayload::Media {
        input: wire(11),
        operation: MediaOperation::Play,
    });
    client.meters("the tone on Master", |meters| master_peak(meters) > LOUD);

    // Choosing a device plays the monitor bus on it as f32le PCM.
    client.device(Some("phones-a"));
    client.output(
        "the headphone device playing",
        AudioMonitorOutputState::Playing,
    );
    let pcm = capture.join("phones-a.pcm");
    let unity = device_reaches(&mut client, &pcm, "the tone on the headphones", |peak| {
        peak > 0.05
    });
    let args = std::fs::read_to_string(capture.join("phones-a.args")).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "-hide_banner",
            "-nostats",
            "-loglevel",
            "error",
            "-f",
            "f32le",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-i",
            "pipe:0",
            "-f",
            "alsa",
            "phones-a"
        ]
    );

    // -6 dB on the headphones halves what the device hears; Program and the
    // monitor meter keep their level.
    client.level(-6_000, false);
    let half = device_reaches(&mut client, &pcm, "the headphones at -6 dB", |peak| {
        peak > unity * 0.4 && peak < unity * 0.6
    });
    assert!(half > 0.0);
    client.meters("Program untouched by the headphone level", |meters| {
        master_peak(meters) > LOUD
            && meters.monitor.as_ref().is_some_and(|monitor| {
                monitor
                    .channels
                    .iter()
                    .any(|channel| channel.peak_millionths > LOUD)
            })
    });

    // Muting the headphones silences only the device.
    client.level(-6_000, true);
    device_reaches(&mut client, &pcm, "muted headphones", |peak| peak == 0.0);
    client.meters("Program still playing", |meters| master_peak(meters) > LOUD);
    client.level(0, false);
    device_reaches(&mut client, &pcm, "the headphones back at unity", |peak| {
        (peak - unity).abs() < unity * 0.1
    });

    // The device is an undoable project edit: undo leaves the bus
    // meter-only, redo plays it on the device again.
    client.accept(CommandPayload::Undo {
        expected_edit: None,
    });
    client.output("the undone device", AudioMonitorOutputState::Off);
    client.accept(CommandPayload::Redo {
        expected_edit: None,
    });
    client.output("the redone device", AudioMonitorOutputState::Playing);
    device_reaches(&mut client, &pcm, "the redone device's tone", |peak| {
        peak > 0.05
    });

    // A device that does not exist fails through the real FFmpeg.
    client.device(Some("hw:CARD=FreemixNoSuchCard,DEV=0"));
    client.output(
        "the missing device failing",
        AudioMonitorOutputState::Failed,
    );

    // Where ALSA is available, the real null device plays.
    if alsa_null_available() {
        client.device(Some("null"));
        client.output(
            "the ALSA null device playing",
            AudioMonitorOutputState::Playing,
        );
        let held = Instant::now();
        while held.elapsed() < Duration::from_millis(500) {
            client.receive();
        }
        client.output(
            "the ALSA null device still playing",
            AudioMonitorOutputState::Playing,
        );
    } else {
        eprintln!("ALSA null device unavailable: real device playback not exercised");
    }

    // No device: meter-only again, and the snapshot carries the level.
    client.device(None);
    client.output("meter-only again", AudioMonitorOutputState::Off);
    drop(client);
    let (_, mixer) = Client::connect(daemon.address);
    assert_eq!(mixer.monitor, fm_protocol::AudioMonitorStatus::default());
}
