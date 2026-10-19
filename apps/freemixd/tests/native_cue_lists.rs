//! RC-017 cue lists on the native daemon, end to end through a real process.
//!
//! - A cue's record actions drive the same native recorder workers as the
//!   direct `record_start`/`record_stop` commands: a cue that starts REC 1
//!   opens a real segment, and a later cue that stops it finalizes the file.
//! - A cue wait survives a SIGKILL: the restarted daemon restores the list's
//!   standby pointer and pending wait from the journal, and its cue clock
//!   (also inside the recorder startup barrier) runs the rest of the cue on
//!   exactly the wait's due engine frame. Show time is engine frames, so the
//!   frames the daemon was down for do not count toward the wait.
//!
//! - Two scripts' waiting runs (RC-011, Protocol 2.69) resume interleaved,
//!   each on its exact due frame, survive a SIGKILL with the same run ids
//!   and due frames, and stop one at a time or all at once.
//!
//! The tests need a native GPU adapter and `FFmpeg`/`ffprobe`. Without an
//! adapter they are skipped unless `FM_REQUIRE_NATIVE_MEDIA=1`; without the
//! tools unless `FM_REQUIRE_FFMPEG=1`.

#![cfg(all(feature = "native-media", unix))]

use std::{
    fs,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    net::{SocketAddr, TcpStream},
    num::NonZeroU128,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use fm_model::{
    Input, InputKind, MainMix, Project, ProjectSettings, RecorderSlot, SimulatedAudio,
    SimulatedInput, SimulatedVideo, SolidColor,
};
use fm_persistence::{ProjectPosition, ProjectStore, RuntimeRouting, StoredProject};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ClientType, CommandMessage, CommandPayload, CommandResult,
    CueListSpec, CueListStateMessage, CueOperation, CueRunChange, EventPayload, HandshakeRequest,
    HeartbeatMessage, Role, ScriptRunStatus, ScriptSpec, ScriptStateMessage, ServerIdentity,
    TransitionFavoriteEffect, WireCue, WireCueAction, WireInputId, WireMessage, WireRecorderSlot,
    decode_line, encode_line,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_TIMEOUT: Duration = Duration::from_secs(30);
/// Well inside the daemon's session timeout.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(3);

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
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG=1 but ffmpeg or ffprobe is unavailable"
    );
    if !available {
        eprintln!("native cue list test skipped: ffmpeg/ffprobe unavailable");
    }
    available
}

/// A 64x48 25 fps generator show; recorder 1 records from startup when
/// `recording`.
fn save_project(path: &Path, recording: bool) {
    let rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(7_017).unwrap()),
        "Native cue list test",
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
    project.set_recording_desired(RecorderSlot::new(1).unwrap(), recording);
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

struct DaemonProcess {
    child: Child,
    address: SocketAddr,
    stderr: mpsc::Receiver<String>,
}

impl DaemonProcess {
    /// Starts `freemixd serve --native-media --record-program <output>`, or
    /// returns `None` when this host has no native media runtime (unless
    /// `FM_REQUIRE_NATIVE_MEDIA=1`).
    fn start(project: &Path, output: &Path) -> Option<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .arg("--native-media")
            .arg("--record-program")
            .arg(output)
            .stdin(Stdio::null())
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
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        });
        let line = ready.recv_timeout(PROCESS_TIMEOUT).unwrap_or_default();
        if let Ok(readiness) = line.trim_end().parse::<ReadinessRecord>() {
            return Some(Self {
                child,
                address: readiness.address,
                stderr,
            });
        }
        let _ = child.kill();
        let _ = child.wait();
        let detail: String = stderr.try_iter().collect();
        let message = format!("native daemon unavailable: {line:?}; stderr={detail}");
        assert!(
            std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
            "FM_REQUIRE_NATIVE_MEDIA=1: {message}"
        );
        eprintln!("native cue list test skipped: {message}");
        None
    }

    fn signal(&self, signal: &str) {
        let status = Command::new("kill")
            .args([signal, &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn wait(mut self, expect_success: bool) -> String {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                let stderr: String = self.stderr.iter().collect();
                if expect_success {
                    assert!(status.success(), "daemon failed: {stderr}");
                }
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

    /// Cooperative stop; returns everything written to stderr.
    fn stop(self) -> String {
        self.signal("-TERM");
        self.wait(true)
    }

    /// Simulated crash: the daemon gets no chance to close anything.
    fn crash(self) -> String {
        self.signal("-KILL");
        self.wait(false)
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let failing = thread::panicking();
        if failing {
            thread::sleep(Duration::from_millis(500));
            eprintln!("daemon exit: {:?}", self.child.try_wait());
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if failing {
            while let Ok(chunk) = self.stderr.recv_timeout(Duration::from_secs(2)) {
                eprint!("{chunk}");
            }
        }
    }
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

/// An admin Studio session that heartbeats while it waits.
struct Client {
    reader: BufReader<TcpStream>,
    /// The start of a line whose rest has not arrived yet.
    pending: Vec<u8>,
    writer: TcpStream,
    server: ServerIdentity,
    heartbeats: u64,
    last_heartbeat: Instant,
    /// The `cue_list_state` records that followed the snapshot.
    lists: Vec<CueListStateMessage>,
    /// The `script_state` records that followed the snapshot.
    scripts: Vec<ScriptStateMessage>,
}

impl Client {
    fn connect(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        writer
            .write_all(
                encode_line(&WireMessage::HandshakeRequest(HandshakeRequest {
                    protocol: CURRENT_PROTOCOL_VERSION,
                    build: "native-cue-list-test".into(),
                    client_type: ClientType::Studio,
                    desired_role: Role::Admin,
                    resume_cursor: None,
                }))
                .unwrap()
                .as_bytes(),
            )
            .unwrap();
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        let mut line = String::new();
        let server = loop {
            assert!(Instant::now() < deadline, "no handshake response");
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => panic!("daemon closed during the handshake"),
                Ok(_) => {}
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    continue;
                }
                Err(error) => panic!("{error}"),
            }
            match decode_line(&line).unwrap() {
                WireMessage::HandshakeResponse(response) => break response.server,
                other => panic!("expected the handshake response, got {other:?}"),
            }
        };
        let mut client = Self {
            reader,
            pending: Vec::new(),
            writer,
            server,
            heartbeats: 0,
            last_heartbeat: Instant::now(),
            lists: Vec::new(),
            scripts: Vec::new(),
        };
        let snapshot = loop {
            if let Some(WireMessage::Snapshot(snapshot)) = client.receive() {
                break snapshot;
            }
            assert!(Instant::now() < deadline, "no snapshot");
        };
        while client.lists.len() < usize::from(snapshot.cue_list_states)
            || client.scripts.len() < usize::from(snapshot.script_states)
        {
            match client.receive() {
                Some(WireMessage::CueListState(state)) => client.lists.push(state),
                Some(WireMessage::ScriptState(state)) => client.scripts.push(state),
                _ => {}
            }
            assert!(Instant::now() < deadline, "missing snapshot state records");
        }
        client
    }

    fn send(&mut self, message: &WireMessage) {
        self.writer
            .write_all(encode_line(message).unwrap().as_bytes())
            .unwrap();
        self.writer.flush().unwrap();
    }

    /// Keeps the session alive like a real client.
    fn heartbeat_if_due(&mut self) {
        if self.last_heartbeat.elapsed() < HEARTBEAT_EVERY {
            return;
        }
        self.heartbeats += 1;
        let heartbeat = WireMessage::Heartbeat(HeartbeatMessage {
            server: self.server.clone(),
            sequence: self.heartbeats,
            sent_at_ms: self.heartbeats,
            last_applied: None,
        });
        self.send(&heartbeat);
        self.last_heartbeat = Instant::now();
    }

    /// The next message, or `None` when no whole line arrived within the
    /// read timeout (the start of a partial line is kept for the next read:
    /// on a loaded host a line can arrive in pieces further apart).
    fn receive(&mut self) -> Option<WireMessage> {
        self.heartbeat_if_due();
        match self.reader.read_until(b'\n', &mut self.pending) {
            Ok(0) => panic!("daemon closed the session"),
            Ok(_) if !self.pending.ends_with(b"\n") => panic!("daemon closed mid-line"),
            Ok(_) => {
                let line = String::from_utf8(std::mem::take(&mut self.pending)).unwrap();
                Some(decode_line(&line).unwrap())
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                None
            }
            Err(error) => panic!("{error}"),
        }
    }

    /// Sends `payload` as command `id` and returns its accepted revision.
    fn command(&mut self, id: &str, payload: CommandPayload) -> u64 {
        self.send(&WireMessage::Command(CommandMessage {
            protocol: CURRENT_PROTOCOL_VERSION,
            id: id.into(),
            idempotency_key: format!("{id}-key"),
            expected_revision: None,
            deadline_ms: None,
            payload,
        }));
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(WireMessage::CommandResult(result)) = self.receive() {
                match result {
                    CommandResult::Accepted {
                        id: accepted,
                        revision,
                        ..
                    } if accepted == id => return revision,
                    other => panic!("expected {id} to be accepted, got {other:?}"),
                }
            }
        }
        panic!("no result for {id}");
    }

    /// The next `cue_run_changed` event.
    fn cue_run(&mut self) -> CueRunChange {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(WireMessage::Event(event)) = self.receive()
                && let EventPayload::CueRunChanged(change) = event.payload
            {
                return *change;
            }
        }
        panic!("timed out waiting for cue_run_changed");
    }

    /// The next script event: `script_ran`, `script_run_changed`, or
    /// `script_runs_stopped` (RC-011).
    fn script_event(&mut self) -> EventPayload {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(WireMessage::Event(event)) = self.receive()
                && matches!(
                    event.payload,
                    EventPayload::ScriptRan(_)
                        | EventPayload::ScriptRunChanged { .. }
                        | EventPayload::ScriptRunsStopped { .. }
                )
            {
                return event.payload;
            }
        }
        panic!("timed out waiting for a script event");
    }

    /// Keeps the session served for `duration`.
    fn idle(&mut self, duration: Duration) {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            self.receive();
        }
    }
}

fn go(client: &mut Client, id: &str) -> CueRunChange {
    client.command(
        id,
        CommandPayload::CueControl {
            list: 1,
            operation: CueOperation::Go { cue: None },
        },
    );
    client.cue_run()
}

fn set_list(client: &mut Client, cues: Vec<WireCue>) {
    client.command(
        "set-cues",
        CommandPayload::SetCueList {
            list: 1,
            spec: Box::new(CueListSpec {
                name: "Show".into(),
                cues,
            }),
        },
    );
}

fn mp4_files(directory: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(directory)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "mp4"))
        .collect();
    files.sort();
    files
}

/// Video frames in a segment, after proving it decodes without complaint.
fn decoded_video_frames(path: &Path) -> u64 {
    let decode = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        decode.status.success() && decode.stderr.is_empty(),
        "{} does not decode cleanly: {}",
        path.display(),
        String::from_utf8_lossy(&decode.stderr)
    );
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        probe.status.success(),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse()
        .unwrap_or_else(|error| panic!("{}: no frame count: {error}", path.display()))
}

fn recorder(start: bool) -> WireCueAction {
    WireCueAction::Record {
        recorder: WireRecorderSlot::new(1).unwrap(),
        start,
    }
}

fn cut(value: u128) -> WireCueAction {
    WireCueAction::Take {
        input: Some(wire(value)),
        effect: TransitionFavoriteEffect::Cut,
        duration_frames: 0,
    }
}

/// A cue that starts REC 1 opens a real segment through the native recorder
/// worker, and a later cue that stops it finalizes the file cleanly.
#[test]
fn cue_record_actions_start_and_finalize_a_native_recording() {
    if !tools_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("cue-record.freemix");
    let recordings = directory.path().join("recordings");
    fs::create_dir_all(&recordings).unwrap();
    let output = recordings.join("program.mp4");
    save_project(&project, false);
    let Some(daemon) = DaemonProcess::start(&project, &output) else {
        return;
    };
    let mut client = Client::connect(daemon.address);
    set_list(
        &mut client,
        vec![
            WireCue {
                id: 10,
                name: "Roll".into(),
                auto_follow: false,
                actions: vec![recorder(true), cut(2)],
            },
            WireCue {
                id: 20,
                name: "Cut".into(),
                auto_follow: false,
                actions: vec![recorder(false)],
            },
        ],
    );
    assert!(mp4_files(&recordings).is_empty(), "nothing records yet");

    let roll = go(&mut client, "roll");
    assert_eq!(roll.fired, vec![10]);
    assert!(roll.recordings[0].desired_active);
    // The cue opened a segment: the file appears while the show runs.
    let deadline = Instant::now() + EVENT_TIMEOUT;
    while mp4_files(&recordings).is_empty() {
        assert!(Instant::now() < deadline, "the cue opened no segment");
        client.idle(Duration::from_millis(100));
    }
    client.idle(Duration::from_secs(2));

    let stop = go(&mut client, "cut");
    assert_eq!(stop.fired, vec![20]);
    assert!(!stop.recordings[0].desired_active);
    // The stop retires the segment off the loop; its single report arrives
    // while the daemon keeps running.
    let mut stderr = String::new();
    let deadline = Instant::now() + EVENT_TIMEOUT;
    while !stderr.contains("FREEMIXD_RECORDER\t") {
        assert!(
            Instant::now() < deadline,
            "the stop cue did not finalize the segment: {stderr}"
        );
        client.idle(Duration::from_millis(100));
        stderr.extend(daemon.stderr.try_iter());
    }
    drop(client);
    stderr.push_str(&daemon.stop());

    let reports: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("FREEMIXD_RECORDER\t"))
        .collect();
    assert_eq!(reports.len(), 1, "{stderr}");
    assert!(
        reports[0].contains("\tslot=1\tstate=Stopped\toutcome=Clean\t"),
        "{}",
        reports[0]
    );
    let files = mp4_files(&recordings);
    assert_eq!(files.len(), 1, "{files:?}");
    let frames = decoded_video_frames(&files[0]);
    assert!(
        frames >= 25,
        "{} holds only {frames} frames",
        files[0].display()
    );
}

/// A SIGKILL mid-wait: the restarted daemon restores the standby pointer
/// and the pending wait, and runs the rest of the cue on exactly its due
/// frame. Recorder 1 records from startup, so the restart passes through the
/// recorder startup barrier with the wait pending.
#[test]
fn a_cue_wait_survives_sigkill_and_completes_on_its_due_frame() {
    const WAIT_FRAMES: u32 = 150;
    if !tools_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("cue-wait.freemix");
    let recordings = directory.path().join("recordings");
    fs::create_dir_all(&recordings).unwrap();
    let output = recordings.join("program.mp4");
    save_project(&project, true);
    let Some(daemon) = DaemonProcess::start(&project, &output) else {
        return;
    };
    let mut client = Client::connect(daemon.address);
    set_list(
        &mut client,
        vec![
            WireCue {
                id: 10,
                name: "Open".into(),
                auto_follow: false,
                actions: vec![
                    cut(2),
                    WireCueAction::Wait {
                        frames: WAIT_FRAMES,
                    },
                    cut(1),
                ],
            },
            WireCue {
                id: 20,
                name: "Close".into(),
                auto_follow: false,
                actions: vec![cut(2)],
            },
        ],
    );
    let fired = go(&mut client, "go");
    assert_eq!(
        (fired.fired.as_slice(), fired.program),
        ([10].as_slice(), wire(2))
    );
    let pending = fired.state.pending.expect("the wait is pending");
    let due = fired.frame + u64::from(WAIT_FRAMES);
    assert_eq!(
        (pending.cue, pending.action, pending.due_frame),
        (10, 2, due)
    );
    client.idle(Duration::from_secs(1));
    drop(client);
    daemon.crash();

    let Some(daemon) = DaemonProcess::start(&project, &output) else {
        panic!("the native daemon started once but not after the crash");
    };
    let mut client = Client::connect(daemon.address);
    let restored = &client.lists[0].list.state;
    assert_eq!(restored.standby, Some(20), "{restored:?}");
    assert_eq!(restored.live, Some(10), "{restored:?}");
    assert_eq!(restored.pending, Some(pending), "{restored:?}");
    let resumed_at = client.lists[0].frame;
    assert!(
        resumed_at < due,
        "resumed at {resumed_at}, past the wait's {due}"
    );

    let advance = client.cue_run();
    assert_eq!(advance.frame, due, "the wait must end on its due frame");
    assert_eq!(advance.program, wire(1));
    assert_eq!(advance.fired, Vec::<u64>::new());
    assert_eq!(advance.state.pending, None);
    assert_eq!(advance.state.standby, Some(20));
    drop(client);
    let stderr = daemon.stop();
    assert!(!stderr.contains("FREEMIXD_CUE\t"), "{stderr}");
}

fn script_ran(client: &mut Client) -> fm_protocol::ScriptRunChange {
    match client.script_event() {
        EventPayload::ScriptRan(change) => *change,
        other => panic!("expected script_ran, got {other:?}"),
    }
}

/// Protocol 2.69 on the native daemon: two scripts' runs wait at once and
/// the cue clock resumes their parts interleaved, each on exactly its due
/// engine frame. A SIGKILL mid-wait loses nothing: journal replay restores
/// both runs with their ids, due frames, and wait lines, and the clock
/// resumes them on those frames. A stop ends one run, then every run.
#[test]
#[allow(clippy::too_many_lines)]
fn two_script_runs_interleave_and_survive_sigkill() {
    if !tools_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("script-runs.freemix");
    let recordings = directory.path().join("recordings");
    fs::create_dir_all(&recordings).unwrap();
    let output = recordings.join("program.mp4");
    save_project(&project, false);
    let Some(daemon) = DaemonProcess::start(&project, &output) else {
        return;
    };
    let mut client = Client::connect(daemon.address);
    for (id, name, source) in [
        (1, "Timer", "cut 2\nwait 40\ncut 1\nwait 75\ncut 2\n"),
        (2, "Score", "wait 15\nmute 1\nwait 400\nmute 2\n"),
    ] {
        client.command(
            &format!("set-{id}"),
            CommandPayload::SetScript {
                script: id,
                spec: Box::new(ScriptSpec {
                    name: name.into(),
                    source: source.into(),
                }),
            },
        );
    }
    client.command("run-1", CommandPayload::RunScript { script: 1 });
    let timer = script_ran(&mut client);
    client.command("run-2", CommandPayload::RunScript { script: 2 });
    let score = script_ran(&mut client);
    assert_eq!((timer.script, score.script), (1, 2));
    assert!(matches!(timer.run, ScriptRunStatus::Waiting { run: 1, .. }));
    assert!(matches!(score.run, ScriptRunStatus::Waiting { run: 2, .. }));

    // Interleaved, each on its exact due frame.
    let mut parts = [script_ran(&mut client), script_ran(&mut client)];
    parts.sort_by_key(|part| part.frame);
    let expected = {
        let mut expected = vec![(1, timer.frame + 40), (2, score.frame + 15)];
        expected.sort_by_key(|(_, frame)| *frame);
        expected
    };
    assert_eq!(
        parts
            .iter()
            .map(|part| (part.script, part.frame))
            .collect::<Vec<_>>(),
        expected
    );
    let waiting: Vec<(u64, ScriptRunStatus)> = {
        let mut waiting: Vec<_> = parts
            .iter()
            .map(|part| (part.script, part.run.clone()))
            .collect();
        waiting.sort_by_key(|(script, _)| *script);
        waiting
    };
    assert!(matches!(
        waiting[0].1,
        ScriptRunStatus::Waiting { run: 1, line: 4, due_frame, .. } if due_frame == timer.frame + 115
    ));
    assert!(matches!(
        waiting[1].1,
        ScriptRunStatus::Waiting { run: 2, line: 3, due_frame, .. } if due_frame == score.frame + 415
    ));
    drop(client);
    daemon.crash();

    let Some(daemon) = DaemonProcess::start(&project, &output) else {
        panic!("the native daemon started once but not after the crash");
    };
    let mut client = Client::connect(daemon.address);
    let restored: Vec<_> = client
        .scripts
        .iter()
        .map(|state| (state.script.id, state.run.clone()))
        .collect();
    assert_eq!(restored, waiting, "journal replay restores both runs");
    // The clock resumes the timer's last part on its due frame; it ends.
    let last = loop {
        let part = script_ran(&mut client);
        if part.script == 1 {
            break part;
        }
    };
    assert_eq!(last.frame, timer.frame + 115);
    assert_eq!(last.run, ScriptRunStatus::Idle);
    // The timer runs again as run 3 alongside the score; a stop ends run 3
    // alone, and a stop of every run then ends the score.
    client.command("rerun-1", CommandPayload::RunScript { script: 1 });
    let again = script_ran(&mut client);
    assert!(matches!(again.run, ScriptRunStatus::Waiting { run: 3, .. }));
    client.command("stop-3", CommandPayload::StopScript { run: Some(3) });
    match client.script_event() {
        EventPayload::ScriptRunChanged { script, run, .. } => {
            assert_eq!((script, run), (1, ScriptRunStatus::Idle));
        }
        other => panic!("expected script_run_changed, got {other:?}"),
    }
    client.command("stop-all", CommandPayload::StopScript { run: None });
    match client.script_event() {
        EventPayload::ScriptRunsStopped { scripts, .. } => assert_eq!(scripts, vec![2]),
        other => panic!("expected script_runs_stopped, got {other:?}"),
    }
    drop(client);
    let stderr = daemon.stop();
    assert!(!stderr.contains("FREEMIXD_CUE\t"), "{stderr}");
}
