//! The simulated daemon's audio render path (AU-005, Protocol 2.75): the
//! persisted per-strip and Master DSP chains are applied to deterministic
//! simulated tones, and `audio_meters` carries the same Master peak, RMS,
//! loudness, and dynamics readings the native daemon reports. A Studio peer
//! demands the meters; a viewer peer receives none, and meter demand never
//! advances the simulated engine or the journal.

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
    AudioDspTarget, AudioDynamicsMeter, AudioLoudnessMeter, AudioMetersMessage,
    AudioMixerOperation, AudioStripDspStateMessage, CURRENT_PROTOCOL_VERSION, ClientType,
    CommandMessage, CommandPayload, CommandResult, HandshakeRequest, Role, SnapshotMessage,
    WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::strip_dsp::{EqBandKind, StripDsp};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

#[path = "support/heartbeat.rs"]
mod heartbeat;
use heartbeat::Heartbeat;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const WAIT: Duration = Duration::from_secs(30);
/// A Master peak this loud (millionths of full scale) is the -18 dBFS tone.
const TONE_PEAK: u32 = 50_000;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn wire(value: u128) -> WireInputId {
    WireInputId::from_domain(input(value))
}

/// Input 1 plays a 440 Hz simulated tone on Program; input 2 is silent.
fn save_project(path: &Path) {
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(57).unwrap()),
        "Simulated strip DSP",
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
        name: "Tone".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(20, 40, 220, 255)),
            SimulatedAudio::Sine { frequency_hz: 440 },
        )),
        required_capabilities: Vec::new(),
    });
    project.add_input(Input {
        id: input(2),
        name: "Silent".into(),
        kind: InputKind::Simulated(SimulatedInput::new(
            SimulatedVideo::Solid(SolidColor::new(220, 40, 20, 255)),
            SimulatedAudio::Silence,
        )),
        required_capabilities: Vec::new(),
    });
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

struct Daemon {
    child: Child,
    address: SocketAddr,
}

impl Daemon {
    /// Starts the simulated daemon (no `--native-media`) on `project`.
    fn start(project: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = sender.send(line);
        });
        let line = receiver.recv_timeout(STARTUP_TIMEOUT).unwrap_or_default();
        let Ok(readiness) = line.parse::<ReadinessRecord>() else {
            let _ = child.kill();
            let _ = child.wait();
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            panic!("simulated daemon did not start: {line:?} {stderr}");
        };
        Self {
            child,
            address: readiness.address,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    heartbeat: Heartbeat,
    commands: u64,
}

impl Client {
    /// Connects and reads the snapshot plus every strip DSP record it
    /// announces.
    fn connect(
        daemon: &Daemon,
        client_type: ClientType,
        role: Role,
    ) -> (Self, SnapshotMessage, Vec<AudioStripDspStateMessage>) {
        let stream = TcpStream::connect(daemon.address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut client = Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            heartbeat: Heartbeat::new(),
            commands: 0,
        };
        client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            build: "sim-strip-dsp-test".into(),
            client_type,
            desired_role: role,
            resume_cursor: None,
        }));
        let WireMessage::HandshakeResponse(_) = client.receive() else {
            panic!("expected the handshake response");
        };
        let WireMessage::Snapshot(snapshot) = client.receive() else {
            panic!("expected the snapshot");
        };
        let mut states = Vec::new();
        while states.len() < usize::from(snapshot.audio_mixer.strip_dsp_states) {
            if let WireMessage::AudioStripDspState(state) = client.receive() {
                assert_eq!(state.revision, snapshot.revision);
                states.push(state);
            }
        }
        (client, snapshot, states)
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

    /// Reads one message, or `None` when `timeout` passes without one.
    fn receive_within(&mut self, timeout: Duration) -> Option<WireMessage> {
        self.reader
            .get_ref()
            .set_read_timeout(Some(timeout))
            .unwrap();
        let available = match self.reader.fill_buf() {
            Ok(bytes) => !bytes.is_empty(),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                false
            }
            Err(error) => panic!("read failed: {error}"),
        };
        self.reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        if !available {
            return None;
        }
        let mut line = String::new();
        assert_ne!(
            self.reader.read_line(&mut line).unwrap(),
            0,
            "daemon closed"
        );
        Some(decode_line(&line).unwrap())
    }

    fn wait_for<T>(&mut self, what: &str, mut found: impl FnMut(WireMessage) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            if let Some(value) = found(self.receive()) {
                return value;
            }
        }
    }

    /// Waits for `count` consecutive meter samples that satisfy `wanted`,
    /// so a DSP glide has settled; returns the last.
    fn settled(
        &mut self,
        what: &str,
        count: usize,
        wanted: impl Fn(&AudioMetersMessage) -> bool,
    ) -> AudioMetersMessage {
        let mut run = 0;
        self.wait_for(what, |message| match message {
            WireMessage::AudioMeters(meters) => {
                if wanted(&meters) {
                    run += 1;
                } else {
                    run = 0;
                }
                (run >= count).then_some(meters)
            }
            _ => None,
        })
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
        self.wait_for("the command result", |message| match message {
            WireMessage::CommandResult(result) => Some(result),
            _ => None,
        })
    }

    fn dsp(&mut self, target: AudioDspTarget, chain: StripDsp) {
        let result = self.submit(CommandPayload::AudioMixer(
            AudioMixerOperation::SetStripDsp {
                target,
                chain: Box::new(chain),
                gesture: None,
            },
        ));
        assert!(
            matches!(&result, CommandResult::Accepted { .. }),
            "{result:?}"
        );
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

fn master_rms(meters: &AudioMetersMessage) -> u32 {
    meters
        .master
        .iter()
        .map(|channel| channel.rms_millionths)
        .max()
        .unwrap_or(0)
}

fn master_loudness(meters: &AudioMetersMessage) -> Option<&AudioLoudnessMeter> {
    meters
        .loudness
        .iter()
        .find(|meter| meter.target == AudioDspTarget::Master)
}

fn master_momentary(meters: &AudioMetersMessage) -> Option<i32> {
    master_loudness(meters).and_then(|meter| meter.momentary_millilufs)
}

fn dynamics(meters: &AudioMetersMessage, target: AudioDspTarget) -> Option<AudioDynamicsMeter> {
    meters
        .dynamics
        .iter()
        .find(|meter| meter.target == target)
        .copied()
}

/// A +12 dB peak at the tone's 440 Hz.
fn tone_boost() -> StripDsp {
    let mut chain = StripDsp::DEFAULT;
    chain.eq.enabled = true;
    let band = &mut chain.eq.bands[2];
    band.enabled = true;
    band.kind = EqBandKind::Peak;
    band.frequency_hz = 440;
    band.gain_millidb = 12_000;
    band.q_milli = 1_000;
    chain
}

/// A Master limiter at -12 dBFS, so the boosted tone is held down.
fn master_limiter() -> StripDsp {
    let mut chain = StripDsp::DEFAULT;
    chain.limiter.enabled = true;
    chain.limiter.ceiling_millidb = -12_000;
    chain
}

#[test]
fn simulated_daemon_meters_its_persisted_simulated_tone() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sim-tone.freemix");
    save_project(&path);
    let daemon = Daemon::start(&path);
    let (mut client, snapshot, states) = Client::connect(&daemon, ClientType::Studio, Role::Admin);
    assert_eq!(snapshot.audio_mixer.master_dsp, StripDsp::DEFAULT);
    assert!(states.is_empty());

    let mut previous: Option<AudioMetersMessage> = None;
    let mut samples = 0_usize;
    let meters = client.wait_for("the simulated tone on Master", |message| {
        let WireMessage::AudioMeters(meters) = message else {
            return None;
        };
        assert!(
            master_peak(&meters) >= TONE_PEAK,
            "the simulated tone should reach Master: {meters:?}"
        );
        assert!(
            master_rms(&meters) > TONE_PEAK / 2,
            "the simulated tone should have RMS: {meters:?}"
        );
        if let Some(previous) = &previous {
            assert!(meters.frame > previous.frame, "{previous:?} -> {meters:?}");
            assert!(
                meters.start_sample > previous.start_sample,
                "{previous:?} -> {meters:?}"
            );
            assert!(
                meters.end_sample > previous.end_sample,
                "{previous:?} -> {meters:?}"
            );
        }
        // The BS.1770 400 ms window fills with the tone, and its interval
        // true peak is reported for Master.
        let reading = master_loudness(&meters).is_some_and(|meter| {
            meter.momentary_millilufs.is_some() && meter.true_peak_millidbtp.is_some()
        });
        previous = Some(meters.clone());
        samples += 1;
        reading.then_some(meters)
    });
    assert!(samples >= 2, "only {samples} meter samples arrived");
    let momentary = master_momentary(&meters).unwrap();
    assert!(
        (-40_000..=-5_000).contains(&momentary),
        "Master momentary loudness {momentary}"
    );
}

#[test]
fn simulated_daemon_applies_a_strip_eq_and_reports_it_in_audio_meters() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sim-strip-dsp.freemix");
    save_project(&path);
    let daemon = Daemon::start(&path);
    let (mut client, _, _) = Client::connect(&daemon, ClientType::Studio, Role::Admin);

    let flat = client.settled("the flat simulated tone", 3, |meters| {
        master_peak(meters) >= TONE_PEAK && master_momentary(meters).is_some()
    });
    let flat_peak = master_peak(&flat);
    let flat_loudness = master_momentary(&flat).unwrap();
    assert_eq!(dynamics(&flat, AudioDspTarget::Master), None);

    // +12 dB at 440 Hz lifts the tone about four times; the Master limiter
    // at -12 dBFS then holds it down and reports its gain reduction.
    client.dsp(AudioDspTarget::Input(wire(1)), tone_boost());
    client.dsp(AudioDspTarget::Master, master_limiter());
    let changed = client.settled("the EQ'd and limited tone", 10, |meters| {
        master_peak(meters) > flat_peak + 50_000
            && master_peak(meters) < flat_peak * 3
            && master_momentary(meters).is_some_and(|value| value > flat_loudness + 3_000)
            && dynamics(meters, AudioDspTarget::Master)
                .and_then(|meter| meter.limiter_millidb)
                .is_some_and(|reduction| reduction > 2_000)
    });
    assert!(
        master_momentary(&changed).unwrap() > flat_loudness,
        "{flat_loudness} -> {:?}",
        master_momentary(&changed)
    );

    // The chains are persisted: a reconnecting Studio peer reads them in its
    // snapshot and still receives the limited meters.
    let (mut reconnected, snapshot, states) =
        Client::connect(&daemon, ClientType::Studio, Role::Admin);
    assert!(snapshot.audio_mixer.master_dsp.limiter.enabled);
    assert_eq!(
        snapshot.audio_mixer.master_dsp.limiter.ceiling_millidb,
        -12_000
    );
    let strip = states
        .iter()
        .find(|state| state.input == wire(1))
        .expect("the edited strip's chain follows the snapshot");
    assert!(strip.chain.eq.enabled);
    assert_eq!(strip.chain.eq.bands[2].gain_millidb, 12_000);
    reconnected.settled("the persisted chain after reconnect", 3, |meters| {
        dynamics(meters, AudioDspTarget::Master)
            .and_then(|meter| meter.limiter_millidb)
            .is_some_and(|reduction| reduction > 2_000)
    });
}

#[test]
fn simulated_daemon_sends_meters_only_to_studio_peers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sim-meter-demand.freemix");
    save_project(&path);
    let daemon = Daemon::start(&path);
    let (mut studio, _, _) = Client::connect(&daemon, ClientType::Studio, Role::Admin);
    let before = studio.settled("the operator's meters", 2, |meters| {
        master_peak(meters) >= TONE_PEAK
    });

    // A viewer peer is served the snapshot but never the meter stream.
    let (mut viewer, _, _) = Client::connect(&daemon, ClientType::Integration, Role::Viewer);
    let after = studio.wait_for("the operator's next meters", |message| match message {
        WireMessage::AudioMeters(meters) if meters.frame > before.frame => Some(meters),
        _ => None,
    });
    assert!(master_peak(&after) >= TONE_PEAK);
    for _ in 0..5 {
        if let Some(message) = viewer.receive_within(Duration::from_millis(200)) {
            assert!(
                !matches!(&message, WireMessage::AudioMeters(_)),
                "a viewer peer received audio meters: {message:?}"
            );
        }
    }
}
