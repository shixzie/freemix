#![cfg(all(feature = "native-media", unix))]

//! Process-level system audio inputs (AU-011, Protocol 2.55): a running
//! native daemon captures a `PulseAudio` source or an ALSA device through an
//! audio-only `FFmpeg` receiver, mixes it on Program like any input, and
//! reports each input's capture state as `system_audio_status` runtime
//! events and snapshot entries.
//!
//! The test puts a wrapper named `ffmpeg` first on the daemon's `PATH`. For
//! the device `freemix_tone` it records the arguments and the audio-server
//! environment the child was given, and replaces the `pulse`/`alsa` device
//! with a 1 kHz `lavfi` tone paced in real time, so the level on Master is
//! known; every other invocation runs the real `FFmpeg`, so a sound card
//! that does not exist keeps reopening with the real error.
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
    AudioMetersMessage, CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload,
    CommandResult, HandshakeRequest, HeartbeatMessage, LiveInputSource, Role,
    RuntimeLifecycleEvent, ServerIdentity, SnapshotMessage, SystemAudioBackend, SystemAudioConfig,
    SystemAudioState, SystemAudioStatus, WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(40);
/// The tone is -6 dBFS: a Master peak between these (millionths of full
/// scale) is the tone at unity.
const TONE_LOW: u32 = 400_000;
const TONE_HIGH: u32 = 600_000;
/// Commands sent by every session of the test.
static COMMANDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The audio-server variable the daemon is started with, which the child
/// must keep.
const PULSE_SERVER: &str = "unix:/run/freemix-test/pulse/native";

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

/// Writes the `ffmpeg` wrapper into `bin`: the `freemix_tone` device's
/// arguments and environment land in `capture`.
fn write_wrapper(bin: &Path, capture: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    let script = format!(
        "#!/bin/sh\n\
         tone=''\n\
         for arg do [ \"$arg\" = freemix_tone ] && tone=1; done\n\
         if [ -n \"$tone\" ]; then\n\
         printf '%s\\n' \"$@\" > '{capture}'/tone.args\n\
         printf 'HOME=%s\\nPULSE_SERVER=%s\\nFREEMIX_LEAK=%s\\n' \"$HOME\" \"$PULSE_SERVER\" \"$FREEMIX_LEAK\" > '{capture}'/tone.env\n\
         for arg do\n\
         shift\n\
         case \"$arg\" in\n\
         pulse|alsa) set -- \"$@\" lavfi ;;\n\
         freemix_tone) set -- \"$@\" 'sine=frequency=1000:sample_rate=48000,volume=4,pan=stereo|c0=c0|c1=c0,arealtime' ;;\n\
         *) set -- \"$@\" \"$arg\" ;;\n\
         esac\n\
         done\n\
         fi\n\
         exec '{real}' \"$@\"\n",
        capture = capture.display(),
        real = real_ffmpeg().display(),
    );
    let path = bin.join("ffmpeg");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Input 1 is a silent blue generator on Preview; input 3, the `PulseAudio`
/// tone, is on Program, so its audio reaches Master.
fn save_project(path: &Path) {
    let store = ProjectStore::new(path).unwrap();
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(55).unwrap()),
        "Native system audio",
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
        name: "Blue".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(20, 40, 220, 255)),
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input(3),
        name: "Desktop".into(),
        kind: InputKind::SystemAudio(SystemAudioConfig::new(
            SystemAudioBackend::Pulse,
            "freemix_tone",
        )),
        required_capabilities: Vec::new(),
    });
    project.set_main_mix(MainMix::new(input(3), input(1)));
    let stored = StoredProject::from_project(
        project,
        RuntimeRouting {
            desired_program_id: Some(input(3)),
            realized_program_id: Some(input(3)),
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

fn start_native(project: &Path, bin: &Path, home: &Path) -> Result<NativeDaemon, String> {
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
        .env("HOME", home)
        .env("PULSE_SERVER", PULSE_SERVER)
        .env("FREEMIX_LEAK", "leaked")
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
    fn connect(address: SocketAddr) -> (Self, SnapshotMessage) {
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
            build: "native-system-audio-test".into(),
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

    /// Waits for a meter sample that satisfies `wanted`.
    fn meters(&mut self, what: &str, wanted: impl Fn(&AudioMetersMessage) -> bool) {
        self.wait_for(what, |_, message| match message {
            WireMessage::AudioMeters(meters) if wanted(&meters) => Some(()),
            _ => None,
        });
    }

    /// Waits for input `value`'s `system_audio_status` that satisfies
    /// `wanted`.
    fn status(
        &mut self,
        what: &str,
        value: u128,
        wanted: impl Fn(&SystemAudioStatus) -> bool,
    ) -> SystemAudioStatus {
        self.wait_for(what, |_, message| match message {
            WireMessage::RuntimeEvent(event) => match event.event {
                RuntimeLifecycleEvent::SystemAudioStatus { input, status }
                    if input == wire(value) && wanted(&status) =>
                {
                    Some(*status)
                }
                _ => None,
            },
            _ => None,
        })
    }

    /// Submits `payload` and returns its result with the events that
    /// arrived before it.
    fn submit(
        &mut self,
        payload: CommandPayload,
    ) -> (CommandResult, Vec<fm_protocol::EventPayload>) {
        // Identifiers stay unique across sessions: the daemon answers a
        // reused idempotency key with the first command's receipt.
        self.commands = COMMANDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let id = format!("command-{}", self.commands);
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id,
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        let mut events = Vec::new();
        let result = self.wait_for("the command result", |_, message| match message {
            WireMessage::CommandResult(result) => Some(result),
            WireMessage::Event(event) => {
                events.push(event.payload);
                None
            }
            _ => None,
        });
        (result, events)
    }

    fn accept(&mut self, payload: CommandPayload) -> Vec<fm_protocol::EventPayload> {
        let described = format!("{payload:?}");
        let (result, events) = self.submit(payload);
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{described}: {result:?}"
        );
        events
    }

    /// Accepts `payload` and waits for the event `wanted` finds.
    fn accept_event(
        &mut self,
        payload: CommandPayload,
        what: &str,
        wanted: impl Fn(&fm_protocol::EventPayload) -> bool,
    ) {
        let events = self.accept(payload);
        if events.iter().any(&wanted) {
            return;
        }
        self.wait_for(what, |_, message| match message {
            WireMessage::Event(event) if wanted(&event.payload) => Some(()),
            _ => None,
        });
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

#[test]
#[allow(clippy::too_many_lines)]
fn native_daemon_captures_system_audio_and_reports_its_state() {
    if !tools_available() {
        eprintln!("native system audio integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let bin = directory.path().join("bin");
    let capture = directory.path().join("capture");
    let home = directory.path().join("home");
    std::fs::create_dir_all(&capture).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    write_wrapper(&bin, &capture);
    let project_path = directory.path().join("native-system-audio.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path, &bin, &home) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native system audio integration skipped: {error}");
            return;
        }
    };
    let (mut client, snapshot) = Client::connect(daemon.address);
    assert_eq!(snapshot.system_audio_inputs.len(), 1);
    assert_eq!(snapshot.system_audio_inputs[0].input, wire(3));
    assert_eq!(
        snapshot.system_audio_inputs[0].config,
        SystemAudioConfig::new(SystemAudioBackend::Pulse, "freemix_tone")
    );

    // The PulseAudio source goes live and its tone plays on Master at its
    // own level.
    let live = if snapshot.system_audio_inputs[0].status.state == SystemAudioState::Live {
        snapshot.system_audio_inputs[0].status.clone()
    } else {
        client.status("the tone live", 3, |status| {
            status.state == SystemAudioState::Live
        })
    };
    assert_eq!(live.state, SystemAudioState::Live);
    client.meters("the tone on Master", |meters| {
        (TONE_LOW..TONE_HIGH).contains(&master_peak(meters))
    });
    // The child opened the PulseAudio source as named, and kept the
    // audio-server environment but nothing else.
    let args = std::fs::read_to_string(capture.join("tone.args")).unwrap();
    let args: Vec<&str> = args.lines().collect();
    assert!(
        args.windows(4)
            .any(|window| window == ["-f", "pulse", "-i", "freemix_tone"]),
        "{args:?}"
    );
    let environment = std::fs::read_to_string(capture.join("tone.env")).unwrap();
    assert_eq!(
        environment,
        format!(
            "HOME={}\nPULSE_SERVER={PULSE_SERVER}\nFREEMIX_LEAK=\n",
            home.display()
        )
    );

    // A sound card that does not exist keeps reopening with the real
    // FFmpeg error, and Program keeps playing the tone.
    client.accept_event(
        CommandPayload::AddInput {
        input: wire(4),
        name: "Missing card".into(),
        source: LiveInputSource::SystemAudio(Box::new(SystemAudioConfig::new(
            SystemAudioBackend::Alsa,
            "hw:CARD=FreemixNoSuchCard,DEV=0",
        ))),
        },
        "the added input",
        |event| matches!(event, fm_protocol::EventPayload::InputAdded { input, .. } if *input == wire(4)),
    );
    let reopening = client.status("the missing card reopening", 4, |status| {
        matches!(status.state, SystemAudioState::Reopening { .. }) && status.last_error.is_some()
    });
    let SystemAudioState::Reopening { attempt, .. } = reopening.state else {
        unreachable!();
    };
    assert!(attempt >= 1);
    client.meters("Program still playing the tone", |meters| {
        (TONE_LOW..TONE_HIGH).contains(&master_peak(meters))
    });

    // A new session's snapshot lists both, the missing card reopening (the
    // native daemon serves one session at a time).
    drop(client);
    let (mut client, snapshot) = Client::connect(daemon.address);
    let inputs: Vec<_> = snapshot
        .system_audio_inputs
        .iter()
        .map(|entry| {
            (
                entry.input,
                entry.config.backend,
                entry.status.last_error.clone(),
            )
        })
        .collect();
    assert_eq!(
        inputs,
        [
            (wire(3), SystemAudioBackend::Pulse, None),
            (wire(4), SystemAudioBackend::Alsa, None),
        ]
    );
    assert!(
        !matches!(
            snapshot.system_audio_inputs[1].status.state,
            SystemAudioState::Live
        ),
        "{:?}",
        snapshot.system_audio_inputs[1].status
    );

    // Removing the input stops its receiver and drops its entry.
    client.accept_event(
        CommandPayload::RemoveInput { input: wire(4) },
        "the removal",
        |event| matches!(event, fm_protocol::EventPayload::InputRemoved { input } if *input == wire(4)),
    );
    drop(client);
    let (mut client, snapshot) = Client::connect(daemon.address);
    assert_eq!(
        snapshot.system_audio_inputs.len(),
        1,
        "{:?} {:?}",
        snapshot.system_audio_inputs,
        snapshot.inputs
    );
    assert_eq!(snapshot.system_audio_inputs[0].input, wire(3));

    // Cutting away from the tone silences Master: its audio follows its
    // picture like any input.
    client.accept(CommandPayload::Cut);
    client.meters("Master silent off Program", |meters| {
        master_peak(meters) < 1_000
    });
}
