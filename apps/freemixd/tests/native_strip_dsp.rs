#![cfg(all(feature = "native-media", unix))]

//! Process-level strip and Master DSP (AU-005, Protocol 2.75): a running
//! native daemon applies a clip strip's EQ, gate, and compressor and the
//! Master limiter to the rendered Program bus, reports their gain reduction
//! in `audio_meters`, and hands a reconnecting client the chains in its
//! snapshot. The loudness reset (Protocol 2.92) restarts one bus's
//! integrated loudness, loudness range, and maximum true peak.
//!
//! The test needs `FFmpeg` and a native GPU adapter (a software Vulkan
//! driver such as lavapipe suffices). Without them it is skipped, unless
//! `FM_REQUIRE_FFMPEG=1` or `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

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
    AudioDspTarget, AudioDynamicsMeter, AudioMetersMessage, AudioMixerOperation,
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    HandshakeRequest, HeartbeatMessage, ImportItemSpec, InputLifecycleOperation, MediaOperation,
    MonitorFrameAssembler, MonitorTap, Role, ServerIdentity, SnapshotMessage, WireInputId,
    WireMessage, decode_line, encode_line,
};
use fm_types::strip_dsp::{EqBandKind, StripDsp};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(40);
/// A Master peak this loud (millionths of full scale) is the clip's tone.
const LOUD: u32 = 50_000;

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

/// A 30 s, 25 fps, 320x180 red clip with a steady 440 Hz tone (peak 1/8).
fn generate_clip(path: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=320x180:r=25:d=30,\
             setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:d=30",
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

/// Inputs 1 and 2 are silent blue generators on Program and Preview.
fn save_project(path: &Path) {
    let store = ProjectStore::new(path).unwrap();
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(55).unwrap()),
        "Native strip DSP",
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

fn start_native(project: &Path) -> Result<NativeDaemon, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
        .arg("serve")
        .arg(project)
        .arg("--native-media")
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
            build: "native-strip-dsp-test".into(),
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

    fn wait_for<T>(&mut self, what: &str, mut found: impl FnMut(WireMessage) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            if let Some(value) = found(self.receive()) {
                return value;
            }
        }
    }

    /// Waits until Program shows the clip's red picture, so it is open.
    fn shows_red_program(&mut self) {
        let mut assembler = MonitorFrameAssembler::new();
        self.wait_for("the clip on Program", |message| {
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

    fn accept(&mut self, payload: CommandPayload) {
        let described = format!("{payload:?}");
        let result = self.submit(payload);
        assert!(
            matches!(result, CommandResult::Accepted { .. }),
            "{described}: {result:?}"
        );
    }

    fn dsp(&mut self, target: AudioDspTarget, chain: StripDsp) {
        self.accept(CommandPayload::AudioMixer(
            AudioMixerOperation::SetStripDsp {
                target,
                chain: Box::new(chain),
                gesture: None,
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

#[test]
#[allow(clippy::too_many_lines)]
fn native_daemon_applies_strip_and_master_dsp_and_meters_gain_reduction() {
    if !tools_available() {
        eprintln!("native strip DSP integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let clip = directory.path().join("clip.mkv");
    generate_clip(&clip);
    let project_path = directory.path().join("native-strip-dsp.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native strip DSP integration skipped: {error}");
            return;
        }
    };
    let (mut client, snapshot) = Client::connect(daemon.address);
    assert_eq!(snapshot.audio_mixer.master_dsp, StripDsp::DEFAULT);
    assert_eq!(snapshot.audio_mixer.strip_dsp_states, 0);

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
    let flat =
        master_peak(&client.settled("the tone on Master", 5, |meters| master_peak(meters) > LOUD));
    let strip = AudioDspTarget::Input(wire(11));

    // BS.1770 loudness and true peak reach clients for Master and the
    // strip once a 400 ms window has been heard.
    let meters = client.settled("loudness on Master and the strip", 3, |meters| {
        [AudioDspTarget::Master, strip].into_iter().all(|target| {
            meters
                .loudness
                .iter()
                .find(|meter| meter.target == target)
                .is_some_and(|meter| {
                    meter.momentary_millilufs.is_some() && meter.true_peak_millidbtp.is_some()
                })
        })
    });
    for meter in &meters.loudness {
        let momentary = meter.momentary_millilufs.unwrap_or(-100_000);
        assert!((-40_000..=-5_000).contains(&momentary), "{meter:?}");
    }

    // EQ: +12 dB at the tone lifts Master about four times; no dynamics
    // stage runs, so none is metered.
    client.dsp(strip, tone_boost());
    let boosted = client.settled("the boosted tone", 10, |meters| {
        f64::from(master_peak(meters)) > f64::from(flat) * 3.2
    });
    assert!(
        f64::from(master_peak(&boosted)) < f64::from(flat) * 4.8,
        "{flat} -> {}",
        master_peak(&boosted)
    );
    assert_eq!(dynamics(&boosted, strip), None);

    // Compressor: -40 dB threshold at 10:1 pulls the tone well down and
    // reports its gain reduction.
    let mut compressed = StripDsp::DEFAULT;
    compressed.compressor.enabled = true;
    compressed.compressor.threshold_millidb = -40_000;
    compressed.compressor.ratio_milli = 10_000;
    compressed.compressor.knee_millidb = 0;
    client.dsp(strip, compressed);
    let meters = client.settled("the compressed tone", 10, |meters| {
        f64::from(master_peak(meters)) < f64::from(flat) * 0.3
            && dynamics(meters, strip)
                .and_then(|meter| meter.compressor_millidb)
                .is_some_and(|reduction| reduction > 10_000)
    });
    let meter = dynamics(&meters, strip).unwrap();
    assert_eq!((meter.gate_millidb, meter.limiter_millidb), (None, None));

    // Gate: a threshold above the tone closes it by its range.
    let mut gated = StripDsp::DEFAULT;
    gated.gate.enabled = true;
    gated.gate.threshold_millidb = -3_000;
    gated.gate.range_millidb = 40_000;
    client.dsp(strip, gated);
    client.settled("the gated tone", 10, |meters| {
        master_peak(meters) < flat / 20
            && dynamics(meters, strip)
                .and_then(|meter| meter.gate_millidb)
                .is_some_and(|reduction| reduction > 30_000)
    });

    // Expander: 8 dB under a -10 dB threshold at 4:1 is turned down by
    // its 20 dB range.
    let mut expanded = StripDsp::DEFAULT;
    expanded.expander.enabled = true;
    expanded.expander.threshold_millidb = -10_000;
    expanded.expander.ratio_milli = 4_000;
    expanded.expander.range_millidb = 20_000;
    client.dsp(strip, expanded);
    client.settled("the expanded tone", 10, |meters| {
        master_peak(meters) < flat / 6
            && dynamics(meters, strip)
                .and_then(|meter| meter.expander_millidb)
                .is_some_and(|reduction| reduction > 15_000)
    });

    // Delay (Protocol 2.95): a one-second strip delay fades into its
    // silent history, so Master falls silent, and the tone arrives a
    // second later.
    let mut delayed = StripDsp::DEFAULT;
    delayed.delay.enabled = true;
    delayed.delay.delay_us = 1_000_000;
    client.dsp(strip, delayed);
    client.settled("Master silent while the delay fills", 1, |meters| {
        master_peak(meters) < flat / 20 && dynamics(meters, strip).is_none()
    });
    client.settled("the delayed tone", 3, |meters| {
        f64::from(master_peak(meters)) > f64::from(flat) * 0.8
    });

    // Master limiter: the boosted tone held under a -20 dBFS ceiling, its
    // reduction metered on Master.
    client.dsp(strip, tone_boost());
    let mut limited = StripDsp::DEFAULT;
    limited.limiter.enabled = true;
    limited.limiter.ceiling_millidb = -20_000;
    client.dsp(AudioDspTarget::Master, limited);
    client.settled("the limited Master", 10, |meters| {
        // -20 dBFS is 100 000 millionths; allow the meter's rounding.
        master_peak(meters) <= 105_000
            && dynamics(meters, AudioDspTarget::Master)
                .and_then(|meter| meter.limiter_millidb)
                .is_some_and(|reduction| reduction > 3_000)
    });

    // Bypass: the strip chain is heard flat again, so the Master limiter
    // has only the plain tone (-18 dBFS) to hold under -20 dBFS.
    let mut bypassed = tone_boost();
    bypassed.bypass = true;
    client.dsp(strip, bypassed);
    client.settled("the bypassed strip", 10, |meters| {
        dynamics(meters, AudioDspTarget::Master)
            .and_then(|meter| meter.limiter_millidb)
            .is_some_and(|reduction| reduction < 4_000)
            && dynamics(meters, strip).is_none()
    });

    // A reconnecting client receives both chains.
    let (mut second, snapshot) = Client::connect(daemon.address);
    assert_eq!(snapshot.audio_mixer.master_dsp, limited);
    assert_eq!(snapshot.audio_mixer.strip_dsp_states, 1);
    let record = second.wait_for("the strip DSP record", |message| match message {
        WireMessage::AudioStripDspState(state) => Some(state),
        _ => None,
    });
    assert_eq!(record.input, wire(11));
    assert_eq!(record.chain, bypassed);

    // Undo restores the boost the bypass replaced.
    client.accept(CommandPayload::Undo {
        expected_edit: None,
    });
    client.settled("the boost after undo", 10, |meters| {
        dynamics(meters, AudioDspTarget::Master)
            .and_then(|meter| meter.limiter_millidb)
            .is_some_and(|reduction| reduction > 10_000)
    });
}

fn loudness(
    meters: &AudioMetersMessage,
    target: AudioDspTarget,
) -> Option<fm_protocol::AudioLoudnessMeter> {
    meters
        .loudness
        .iter()
        .find(|meter| meter.target == target)
        .copied()
}

/// Imports the clip, takes it to Program, and plays it.
fn play_clip(client: &mut Client, clip: &Path) {
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
}

#[test]
fn a_loudness_reset_restarts_one_bus_measurement_on_a_running_daemon() {
    if !tools_available() {
        eprintln!("native loudness reset integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let clip = directory.path().join("clip.mkv");
    generate_clip(&clip);
    let project_path = directory.path().join("native-loudness-reset.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native loudness reset integration skipped: {error}");
            return;
        }
    };
    let (mut client, _) = Client::connect(daemon.address);
    play_clip(&mut client, &clip);
    let strip = AudioDspTarget::Input(wire(11));

    // A +12 dB boost raises the highest true peak on Master and the strip;
    // the flat tone that follows leaves it there until a reset.
    client.settled("integrated loudness on Master", 3, |meters| {
        loudness(meters, AudioDspTarget::Master)
            .is_some_and(|meter| meter.integrated_millilufs.is_some())
    });
    client.dsp(strip, tone_boost());
    let boosted = client.settled("the boosted true peak", 3, |meters| {
        loudness(meters, AudioDspTarget::Master)
            .and_then(|meter| meter.true_peak_millidbtp)
            .is_some_and(|peak| peak > -12_000)
    });
    let high = loudness(&boosted, AudioDspTarget::Master)
        .and_then(|meter| meter.max_true_peak_millidbtp)
        .unwrap();
    client.dsp(strip, StripDsp::DEFAULT);
    client.settled("the flat tone again", 5, |meters| {
        loudness(meters, AudioDspTarget::Master)
            .and_then(|meter| meter.true_peak_millidbtp)
            .is_some_and(|peak| peak < high - 9_000)
    });

    // Resetting Master restarts its maximum true peak at the flat tone and
    // its integrated loudness and range from the flat tone alone; the
    // strip's own measurement still holds the boost.
    client.accept(CommandPayload::AudioMixer(
        AudioMixerOperation::ResetLoudness {
            target: AudioDspTarget::Master,
        },
    ));
    let meters = client.settled("Master's measurement restarted", 3, |meters| {
        loudness(meters, AudioDspTarget::Master).is_some_and(|meter| {
            meter
                .max_true_peak_millidbtp
                .is_some_and(|peak| peak < high - 9_000)
                && meter
                    .loudness_range_millilu
                    .is_some_and(|range| range < 1_000)
        })
    });
    let master = loudness(&meters, AudioDspTarget::Master).unwrap();
    let short_term = master.short_term_millilufs.unwrap();
    assert!(
        (master.integrated_millilufs.unwrap() - short_term).abs() < 1_000,
        "{master:?}"
    );
    let held = loudness(&meters, strip)
        .and_then(|meter| meter.max_true_peak_millidbtp)
        .unwrap();
    assert!(held > high - 3_000, "{held} after {high}");

    // Resetting the strip restarts its own measurement too.
    client.accept(CommandPayload::AudioMixer(
        AudioMixerOperation::ResetLoudness { target: strip },
    ));
    client.settled("the strip's measurement restarted", 3, |meters| {
        loudness(meters, strip)
            .and_then(|meter| meter.max_true_peak_millidbtp)
            .is_some_and(|peak| peak < high - 9_000)
    });
}
