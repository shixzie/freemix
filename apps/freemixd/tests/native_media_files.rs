#![cfg(feature = "native-media")]

//! Process-level IN-003 on the native runtime: a native daemon plays a
//! media-file input through the engine's transport, decoded by its file
//! player off the render loop. The Program monitor shows exactly the frame
//! the transport presents (the fixture draws each frame's number as bit
//! blocks), and `media_state`, `media_changed`, and `media` runtime events
//! report the decoder's source and the transport's position, end action,
//! and audio-track changes.
//!
//! IN-026 on the same runtime: pausing the input holds its picture while
//! its transport plays, and closing it releases the file's decoder until it
//! reopens.
//!
//! The test needs `FFmpeg` tools and a native GPU adapter (a software Vulkan
//! driver such as lavapipe suffices). Without them it is skipped, unless
//! `FM_REQUIRE_FFMPEG=1` or `FM_REQUIRE_NATIVE_MEDIA=1` makes that a failure.

use std::{
    fs,
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
    Input, InputKind, MainMix, MediaPlaybackSettings, Project, ProjectSettings, SimulatedAudio,
    SimulatedInput, SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    EventPayload, HandshakeRequest, MediaEndAction, MediaEvent, MediaOperation, MediaSeekTarget,
    MediaStatus, MediaTransportState, MonitorFrameAssembler, MonitorImage, Role,
    RuntimeLifecycleEvent, WireInputId, WireMessage, decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT: Duration = Duration::from_secs(30);
const CLIP: &str = "clip.mkv";

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn clip() -> WireInputId {
    WireInputId::from_domain(input(2))
}

/// Whether `FFmpeg` tools are on `PATH`; `FM_REQUIRE_FFMPEG=1` makes their
/// absence a failure.
fn ffmpeg_available() -> bool {
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

/// A 4 s, 25 fps, 320x180 Matroska clip: each frame draws its number (mod
/// 256) as eight 40-pixel-wide black/white bit blocks, low bit left; audio
/// track 1 (English, mono) and track 2 (Spanish, stereo) are 48 kHz PCM.
fn generate_clip(path: &Path) {
    let video = "color=c=black:s=320x180:r=25:d=4,format=gray,\
                 geq=lum='if(mod(floor(N/pow(2\\,floor(X/40)))\\,2)\\,235\\,16)',\
                 setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv";
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", video])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:d=4",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "aevalsrc='sin(2*PI*880*t)|0.25':s=48000:d=4",
        ])
        .args(["-map", "0:v", "-map", "1:a", "-map", "2:a"])
        .args(["-c:v", "mpeg2video", "-g", "12", "-bf", "2", "-q:v", "2"])
        .args(["-pix_fmt", "yuv420p", "-c:a", "pcm_f32le"])
        .args([
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
        ])
        .args(["-metadata:s:a:0", "language=eng"])
        .args(["-metadata:s:a:1", "language=spa"])
        .args(["-f", "matroska", "-y"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "fixture generation failed: {status}");
}

/// Input 1 is a silent solid generator on Preview, input 2 the clip on
/// Program with default playback settings.
fn save_project(path: &Path) {
    let store = ProjectStore::new(path).unwrap();
    fs::create_dir_all(store.assets_root()).unwrap();
    generate_clip(&store.assets_root().join(CLIP));
    let frame_rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(43).unwrap()),
        "Native media files",
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
        name: "Clip".into(),
        kind: InputKind::Media {
            asset_uri: format!("asset://{CLIP}"),
            playback: MediaPlaybackSettings::default(),
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

/// A started native daemon, killed and reaped on drop.
struct NativeDaemon {
    child: Child,
    address: SocketAddr,
}

impl Drop for NativeDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
        // Drain stderr so the daemon never blocks on a full pipe.
        if let Some(mut stderr) = child.stderr.take() {
            thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = stderr.read_to_end(&mut sink);
            });
        }
        return Ok(NativeDaemon {
            child,
            address: readiness.address,
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
        let mut line = String::new();
        assert_ne!(
            self.reader.read_line(&mut line).unwrap(),
            0,
            "daemon closed"
        );
        decode_line(&line).unwrap()
    }

    /// Reads messages until `found` picks one, failing after [`WAIT`].
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

    /// The next Program monitor picture, decoded to the clip frame it shows.
    fn shown_frame(&mut self, message: WireMessage) -> Option<u64> {
        let WireMessage::MonitorFrame(part) = message else {
            return None;
        };
        let image = self.assembler.push(&part).unwrap()?;
        (image.input == clip()).then(|| frame_number(&image))
    }

    /// Submits `payload` and waits for its acceptance.
    fn accept(&mut self, payload: CommandPayload) {
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
        self.wait_for("the command result", |_, message| match message {
            WireMessage::CommandResult(result) => {
                assert!(
                    matches!(result, CommandResult::Accepted { .. }),
                    "{described}: {result:?}"
                );
                Some(())
            }
            _ => None,
        });
    }

    /// The clip's transport as the next `media` runtime event reports it.
    fn next_transport(&mut self, what: &str) -> fm_protocol::MediaTransportStatus {
        self.wait_for(what, |_, message| {
            let WireMessage::RuntimeEvent(event) = message else {
                return None;
            };
            let RuntimeLifecycleEvent::Media { status, .. } = event.event else {
                return None;
            };
            (status.input == clip()).then_some(status.transport)
        })
    }

    /// Submits a media operation on the clip and waits for its acceptance,
    /// returning the `media_changed` status.
    fn media(&mut self, operation: MediaOperation) -> MediaStatus {
        self.commands += 1;
        let id = format!("media-{}", self.commands);
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.clone(),
            idempotency_key: id.clone(),
            expected_revision: None,
            deadline_ms: None,
            payload: CommandPayload::Media {
                input: clip(),
                operation,
            },
        }));
        let mut accepted = false;
        let mut status = None;
        self.wait_for("the media command", |_, message| {
            match message {
                WireMessage::CommandResult(result) => {
                    assert!(
                        matches!(result, CommandResult::Accepted { .. }),
                        "{operation:?}: {result:?}"
                    );
                    accepted = true;
                }
                WireMessage::Event(event) => {
                    if let EventPayload::MediaChanged {
                        operation: changed,
                        status: changed_status,
                        ..
                    } = event.payload
                    {
                        assert_eq!(changed, operation);
                        status = Some(*changed_status);
                    }
                }
                _ => {}
            }
            (accepted && status.is_some()).then(|| status.clone().unwrap())
        })
    }
}

/// The frame number (mod 256) the picture's bit blocks encode.
fn frame_number(image: &MonitorImage) -> u64 {
    let width = usize::from(image.width);
    (0..8).fold(0, |value, bit| {
        let x = bit * 40 + 20;
        let red = image.rgba[(90 * width + x) * 4];
        if red > 128 { value | (1 << bit) } else { value }
    })
}

#[test]
#[allow(clippy::too_many_lines)]
fn native_daemon_plays_a_media_file_through_its_engine_transport() {
    if !ffmpeg_available() {
        eprintln!("native media file integration skipped: FFmpeg tools unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project_path = directory.path().join("native-media-files.freemix");
    save_project(&project_path);
    let daemon = match start_native(&project_path) {
        Ok(daemon) => daemon,
        Err(error) => {
            assert!(
                std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
                "FM_REQUIRE_NATIVE_MEDIA=1: {error}"
            );
            eprintln!("native media file integration skipped: {error}");
            return;
        }
    };
    let mut client = Client::connect(daemon.address);
    client.send(&WireMessage::HandshakeRequest(HandshakeRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        build: "native-media-files-test".into(),
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
    assert_eq!(snapshot.media_states, 1);

    // The snapshot's media state carries the decoder's source: 100 frames
    // at 25 fps, two audio tracks with their languages, cued and stopped.
    let state = client.wait_for("the media state", |_, message| match message {
        WireMessage::MediaState(state) => Some(state),
        _ => None,
    });
    let transport = &state.status.transport;
    assert_eq!(state.status.input, clip());
    assert!(transport.available, "{transport:?}");
    assert_eq!(transport.state, MediaTransportState::Stopped);
    assert_eq!((transport.position, transport.frames), (0, 100));
    assert!(transport.complete);
    assert_eq!(transport.source_rate, Some(FrameRate::new(25, 1).unwrap()));
    let languages = transport
        .tracks
        .iter()
        .map(|track| (track.stream_index, track.language.as_deref()))
        .collect::<Vec<_>>();
    assert_eq!(languages, [(1, Some("eng")), (2, Some("spa"))]);
    assert_eq!(transport.fault, None);

    // Program shows the cued first frame.
    client.wait_for("frame 0 on Program", |client, message| {
        (client.shown_frame(message)? == 0).then_some(())
    });

    // A frame-exact seek shows exactly that frame.
    let seeked = client.media(MediaOperation::Seek(MediaSeekTarget::Frame(40)));
    assert_eq!(seeked.transport.position, 40);
    client.wait_for("frame 40 on Program", |client, message| {
        (client.shown_frame(message)? == 40).then_some(())
    });

    // Playing advances the picture and the reported position in step.
    let playing = client.media(MediaOperation::Play);
    assert_eq!(playing.transport.state, MediaTransportState::Playing);
    let mut shown = Vec::new();
    let mut positions = Vec::new();
    client.wait_for("playback past frame 60", |client, message| {
        if let WireMessage::RuntimeEvent(event) = &message
            && let RuntimeLifecycleEvent::Media { status, .. } = &event.event
        {
            positions.push(status.transport.position);
        }
        let frame = client.shown_frame(message)?;
        shown.push(frame);
        (frame > 60).then_some(())
    });
    assert!(
        shown.windows(2).all(|pair| pair[0] <= pair[1]),
        "frames went backwards while playing: {shown:?}"
    );
    assert!(
        positions.windows(2).all(|pair| pair[0] <= pair[1]),
        "{positions:?}"
    );

    // Looping at the end: the transport reports the loop and Program wraps
    // to the start.
    let looping = client.media(MediaOperation::EndAction(MediaEndAction::Loop));
    assert_eq!(looping.settings.end_action, MediaEndAction::Loop);
    client.media(MediaOperation::Seek(MediaSeekTarget::Frame(95)));
    let mut looped = false;
    client.wait_for("a loop back to the start", |client, message| {
        if let WireMessage::RuntimeEvent(event) = &message
            && let RuntimeLifecycleEvent::Media { events, .. } = &event.event
        {
            looped |= events.contains(&MediaEvent::Looped { count: 1 });
        }
        let frame = client.shown_frame(message)?;
        (looped && frame < 20).then_some(())
    });

    // Switching the audio track is reported with both tracks.
    let switched = client.media(MediaOperation::AudioTrack(Some(2)));
    assert_eq!(switched.transport.audio_track, Some(2));
    client.wait_for("the audio track change", |_, message| {
        let WireMessage::RuntimeEvent(event) = message else {
            return None;
        };
        let RuntimeLifecycleEvent::Media { events, .. } = event.event else {
            return None;
        };
        events
            .contains(&MediaEvent::AudioTrackChanged {
                from: Some(1),
                to: Some(2),
            })
            .then_some(())
    });

    // Pausing holds the picture: once monitor frames rendered before the
    // pause drain, Program repeats one frame.
    let paused = client.media(MediaOperation::Pause);
    assert_eq!(paused.transport.state, MediaTransportState::Paused);
    let mut last = None;
    let mut repeats = 0;
    let held = client.wait_for("the picture to hold", |client, message| {
        let frame = client.shown_frame(message)?;
        if last == Some(frame) {
            repeats += 1;
        } else {
            last = Some(frame);
            repeats = 0;
        }
        (repeats == 3).then_some(frame)
    });
    let mut repeated = 0;
    client.wait_for("the held frame twice more", |client, message| {
        let frame = client.shown_frame(message)?;
        assert_eq!(frame, held, "paused at {held}, showed {frame}");
        repeated += 1;
        (repeated == 2).then_some(())
    });

    // IN-026: pausing the input holds its picture although its transport
    // is playing.
    client.media(MediaOperation::Play);
    client.wait_for("playback after the pause", |client, message| {
        let frame = client.shown_frame(message)?;
        (frame != held).then_some(())
    });
    client.accept(CommandPayload::InputLifecycle(
        fm_protocol::InputLifecycleOperation::SetPaused {
            input: clip(),
            paused: true,
        },
    ));
    let mut last = None;
    let mut repeats = 0;
    let held = client.wait_for("the paused input to hold", |client, message| {
        let frame = client.shown_frame(message)?;
        if last == Some(frame) {
            repeats += 1;
        } else {
            last = Some(frame);
            repeats = 0;
        }
        (repeats == 5).then_some(frame)
    });
    let mut repeated = 0;
    client.wait_for("the paused input's frame twice more", |client, message| {
        let frame = client.shown_frame(message)?;
        assert_eq!(
            frame, held,
            "the paused input held {held}, then showed {frame}"
        );
        repeated += 1;
        (repeated == 2).then_some(())
    });
    client.accept(CommandPayload::InputLifecycle(
        fm_protocol::InputLifecycleOperation::SetPaused {
            input: clip(),
            paused: false,
        },
    ));
    client.wait_for("playback after resuming", |client, message| {
        let frame = client.shown_frame(message)?;
        (frame != held).then_some(())
    });

    // Closing the input (off Program) releases its source; reopening opens
    // the file again on a fresh transport.
    client.accept(CommandPayload::Cut);
    client.accept(CommandPayload::InputLifecycle(
        fm_protocol::InputLifecycleOperation::SetClosed {
            input: clip(),
            closed: true,
        },
    ));
    let mut closed = client.next_transport("the closed transport");
    while closed.available {
        closed = client.next_transport("the closed transport");
    }
    client.accept(CommandPayload::InputLifecycle(
        fm_protocol::InputLifecycleOperation::SetClosed {
            input: clip(),
            closed: false,
        },
    ));
    let seeked = client.media(MediaOperation::Seek(MediaSeekTarget::Frame(10)));
    assert!(seeked.transport.available, "{:?}", seeked.transport);
    assert_eq!(seeked.transport.fault, None);
    assert_eq!(seeked.transport.frames, 100);
    drop(client);
    drop(daemon);
}
