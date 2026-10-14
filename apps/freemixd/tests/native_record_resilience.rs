//! Fault-tolerant native program recording end to end: a real daemon, a real
//! `FFmpeg`, and a recording directory that fails or a daemon that dies.
//!
//! Each test skips when `ffmpeg`/`ffprobe` are unavailable unless
//! `FM_REQUIRE_FFMPEG=1`, and when this host has no native media runtime
//! unless `FM_REQUIRE_NATIVE_MEDIA=1`.

#![cfg(all(feature = "native-media", unix))]

use std::{
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Read, Write},
    num::NonZeroU128,
    os::unix::fs::PermissionsExt,
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
use fm_record::{Fmp4Integrity, SessionManifest, SessionState, scan_fmp4_file};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};
use freemixd::ReadinessRecord;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT_TIMEOUT: Duration = Duration::from_secs(60);

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
        eprintln!("native record resilience test skipped: ffmpeg/ffprobe unavailable");
    }
    available
}

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

/// A 64x48 generator show whose first recorder slot records from startup.
fn save_recording_project(path: &Path) {
    let rate = FrameRate::new(25, 1).unwrap();
    let mut project = Project::new(
        ProjectId::new(NonZeroU128::new(7_301).unwrap()),
        "Native record resilience test",
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
    project.set_recording_desired(RecorderSlot::new(1).unwrap(), true);
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
    stderr: mpsc::Receiver<String>,
}

impl DaemonProcess {
    /// Starts a headless native daemon that records from startup, or returns
    /// `None` when this host has no native media runtime (unless
    /// `FM_REQUIRE_NATIVE_MEDIA=1`).
    fn start(project: &Path, arguments: &[OsString]) -> Option<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_freemixd"))
            .arg("serve")
            .arg(project)
            .arg("--native-media")
            .args(arguments)
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
        if line.trim_end().parse::<ReadinessRecord>().is_ok() {
            return Some(Self { child, stderr });
        }
        let _ = child.kill();
        let _ = child.wait();
        let detail: String = stderr.try_iter().collect();
        let message = format!("native daemon unavailable: {line:?}; stderr={detail}");
        assert!(
            std::env::var("FM_REQUIRE_NATIVE_MEDIA").as_deref() != Ok("1"),
            "FM_REQUIRE_NATIVE_MEDIA=1: {message}"
        );
        eprintln!("native record resilience test skipped: {message}");
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

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

fn mp4_files(directory: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|extension| extension == "mp4"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |metadata| metadata.len())
}

/// Video frames in a segment, after proving it decodes without a single
/// decoder or demuxer complaint.
fn decoded_video_frames(path: &Path) -> u64 {
    let decode = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"])
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

/// The newest intact copy of session `name`'s manifest across `roots`.
fn newest_manifest(name: &str, roots: &[&Path]) -> SessionManifest {
    roots
        .iter()
        .filter_map(|root| fs::read_to_string(root.join(format!("{name}.fmsession"))).ok())
        .filter_map(|text| SessionManifest::parse(&text).ok())
        .max_by_key(|manifest| manifest.generation)
        .unwrap_or_else(|| panic!("no intact manifest for session {name}"))
}

fn lines<'a>(stderr: &'a str, prefix: &str) -> Vec<&'a str> {
    stderr
        .lines()
        .filter(|line| line.starts_with(&format!("{prefix}\t")))
        .collect()
}

fn os(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

/// The primary recording directory turns read-only mid-show. The segment
/// already open keeps writing (its descriptor stays valid), and every later
/// segment fails over to `--record-fallback-dir`: the recording continues,
/// every segment on both directories decodes, and every session closes.
#[test]
#[allow(clippy::too_many_lines)]
fn a_primary_made_unwritable_mid_recording_continues_in_the_fallback() {
    if !tools_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("resilience.freemix");
    save_recording_project(&project);
    let primary = directory.path().join("primary");
    let fallback = directory.path().join("fallback");
    fs::create_dir_all(&primary).unwrap();
    let mut arguments = os(&["--record-program"]);
    arguments.push(primary.join("program.mp4").into());
    arguments.extend(os(&[
        "--record-segment-seconds",
        "1",
        "--record-fallback-dir",
    ]));
    arguments.push(fallback.clone().into());
    let Some(daemon) = DaemonProcess::start(&project, &arguments) else {
        return;
    };

    // At least one full segment lands on the primary before it fails.
    wait_until("a rotated primary segment", || {
        file_len(&primary.join("program-002.mp4")) > 0
    });
    fs::set_permissions(&primary, fs::Permissions::from_mode(0o555)).unwrap();
    let permissions_bind = fs::File::create(primary.join("probe")).is_err();
    if permissions_bind {
        wait_until("two fallback segments with data", || {
            mp4_files(&fallback)
                .iter()
                .filter(|path| file_len(path) > 0)
                .count()
                >= 2
        });
    }
    let stderr = daemon.stop();
    fs::set_permissions(&primary, fs::Permissions::from_mode(0o755)).unwrap();
    if !permissions_bind {
        eprintln!("native record resilience test skipped: permissions do not bind for this user");
        return;
    }

    let reports = lines(&stderr, "FREEMIXD_RECORDER");
    assert!(reports.len() >= 4, "{stderr}");
    for report in &reports {
        assert!(
            report.contains("\toutcome=Clean\t") && report.contains("\tfailure=none"),
            "a failover is not a recorder failure: {report}\n{stderr}"
        );
    }
    let failovers = lines(&stderr, "FREEMIXD_RECORDER_FAILOVER");
    assert!(
        failovers
            .iter()
            .any(|line| line.contains("\tactive_root=1\tdisabled_roots=0\t")
                && line.contains("PermissionDenied")),
        "{stderr}"
    );
    assert!(
        lines(&stderr, "FREEMIXD_RECORDER_FAILURE").is_empty(),
        "{stderr}"
    );
    let sessions = lines(&stderr, "FREEMIXD_RECORDER_SESSION");
    assert_eq!(sessions.len(), reports.len(), "{stderr}");
    assert!(
        sessions
            .iter()
            .all(|line| line.contains("\tstate=Stopped\tfinished=true\t")),
        "{stderr}"
    );
    assert!(
        !lines(&stderr, "FREEMIXD_RECORDER_SPACE").is_empty(),
        "{stderr}"
    );

    let primary_segments = mp4_files(&primary);
    let fallback_segments = mp4_files(&fallback);
    assert!(primary_segments.len() >= 2, "{primary_segments:?}");
    assert!(fallback_segments.len() >= 2, "{fallback_segments:?}");
    let mut fallback_frames = 0;
    for segment in primary_segments.iter().chain(&fallback_segments) {
        let frames = decoded_video_frames(segment);
        assert!(frames > 0, "{} holds no video", segment.display());
        if segment.starts_with(&fallback) {
            fallback_frames += frames;
        }
    }
    assert!(
        fallback_frames >= 25,
        "the recording continued in the fallback for at least a second: {fallback_frames}"
    );

    // Every session's newest manifest copy is closed, whichever directory
    // still held it.
    let mut names: Vec<String> = [&primary, &fallback]
        .iter()
        .flat_map(|directory| fs::read_dir(directory).unwrap().flatten())
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()?
                .strip_suffix(".fmsession")
                .map(str::to_owned)
        })
        .collect();
    names.sort();
    names.dedup();
    assert!(names.len() >= 4, "{names:?}");
    for name in names {
        let manifest = newest_manifest(&name, &[&primary, &fallback]);
        assert_eq!(manifest.state, SessionState::Stopped, "session {name}");
    }
}

/// The daemon dies mid-recording and its last write was torn. The next start
/// recovers the session before recording again: the torn tail is cut back to
/// the last complete fragment, the segment decodes, the manifest says
/// recovered, the loss is reported, and the recovered name is not reused.
#[test]
fn startup_recovery_repairs_a_segment_torn_by_a_daemon_crash() {
    if !tools_available() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("crash.freemix");
    save_recording_project(&project);
    let output = directory.path().join("program.mp4");
    let mut arguments = os(&["--record-program"]);
    arguments.push(output.clone().into());
    let Some(daemon) = DaemonProcess::start(&project, &arguments) else {
        return;
    };
    // A few fragments (one per second) reach the segment before the crash.
    wait_until("three recorded fragments", || {
        fs::read_to_string(directory.path().join("program.fmsession")).is_ok()
            && scan_fmp4_file(&output).is_ok_and(|scan| scan.fragments >= 3)
    });
    let crashed = daemon.crash();
    assert!(
        lines(&crashed, "FREEMIXD_RECORDER").is_empty(),
        "a crashed daemon reports nothing: {crashed}"
    );
    // The orphaned muxer exits once its inputs vanish; it never held the
    // segment, so nothing else writes to it.
    let complete = scan_fmp4_file(&output).unwrap();
    assert_eq!(complete.integrity, Fmp4Integrity::Complete);
    let fragments = complete.fragments;
    let good_length = complete.valid_length;
    assert_eq!(
        newest_manifest("program", &[directory.path()]).state,
        SessionState::Recording,
        "the crash left the session open"
    );
    // The crash tore the write in flight: half a `moof` header.
    let mut file = fs::OpenOptions::new().append(true).open(&output).unwrap();
    file.write_all(&[0, 0, 0x10, 0, b'm', b'o', b'o', b'f', 0, 0, 0])
        .unwrap();
    drop(file);
    assert_ne!(
        scan_fmp4_file(&output).unwrap().integrity,
        Fmp4Integrity::Complete,
        "the segment is torn"
    );

    let Some(daemon) = DaemonProcess::start(&project, &arguments) else {
        return;
    };
    let stderr = daemon.stop();
    let recovery = lines(&stderr, "FREEMIXD_RECORDING_RECOVERY");
    assert_eq!(recovery.len(), 1, "{stderr}");
    let record = recovery[0];
    for field in [
        "\tslot=1\t",
        "\tsession=program\t",
        "\tstatus=recovered\t",
        "\tprevious_state=Recording\t",
        "\tsegments=1\t",
        "\trepaired=1\t",
        "\tunrecoverable=0\t",
        "\tmissing=0\t",
        &format!("\tkept_bytes={good_length}\t"),
        "\tdiscarded_bytes=11\t",
    ] {
        assert!(record.contains(field), "missing {field:?} in {record}");
    }
    // Recovery runs before the restarted daemon records anything.
    let recovery_at = stderr.find("FREEMIXD_RECORDING_RECOVERY").unwrap();
    let space_at = stderr.find("FREEMIXD_RECORDER_SPACE").unwrap();
    assert!(recovery_at < space_at, "{stderr}");

    assert_eq!(file_len(&output), good_length);
    let repaired = scan_fmp4_file(&output).unwrap();
    assert_eq!(repaired.integrity, Fmp4Integrity::Complete);
    assert_eq!(repaired.fragments, fragments);
    assert!(decoded_video_frames(&output) >= 50);
    assert_eq!(
        newest_manifest("program", &[directory.path()]).state,
        SessionState::Recovered
    );
    // The restarted daemon recorded into a new name and closed it.
    let next = directory.path().join("program-002.mp4");
    assert!(decoded_video_frames(&next) > 0);
    assert_eq!(
        newest_manifest("program-002", &[directory.path()]).state,
        SessionState::Stopped
    );

    // A third start finds nothing left to recover.
    let Some(daemon) = DaemonProcess::start(&project, &arguments) else {
        return;
    };
    let stderr = daemon.stop();
    assert!(
        lines(&stderr, "FREEMIXD_RECORDING_RECOVERY").is_empty(),
        "{stderr}"
    );
}
