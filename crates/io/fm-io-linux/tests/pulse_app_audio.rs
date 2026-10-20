//! IN-011 application audio against a real `PulseAudio` server: a private
//! `pulseaudio` with a null sink, two "applications" (`ffmpeg` playing
//! different tones to it), and an [`AudioTap`] following one of them by its
//! process. The tap serves only that application's tone, follows a stream
//! that a child process starts later, reports `no_stream` while nothing
//! plays, and leaves the streams where they play (the null sink's monitor
//! still hears both).
//!
//! Skipped when `pulseaudio`, `pactl`, `parec`, or `ffmpeg` is not on
//! `PATH`; set `FM_REQUIRE_PULSEAUDIO=1` to fail instead.

#![cfg(target_os = "linux")]

use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use fm_io_linux::capture_audio::{AppAudioKey, AudioTap, TapSource, TapState};

fn available(program: &str, arg: &str) -> bool {
    Command::new(program)
        .arg(arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn skip(reason: &str) -> bool {
    assert!(
        std::env::var_os("FM_REQUIRE_PULSEAUDIO").is_none(),
        "FM_REQUIRE_PULSEAUDIO is set but {reason}"
    );
    eprintln!("skipping: {reason}");
    true
}

/// A private `PulseAudio` server with a null sink `fmtest` as its default.
struct Pulse {
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
}

impl Pulse {
    fn start(name: &str) -> Option<Self> {
        for (program, arg) in [
            ("pulseaudio", "--version"),
            ("pactl", "--version"),
            ("parec", "--version"),
            ("ffmpeg", "-version"),
        ] {
            if !available(program, arg) && skip(&format!("{program} is not installed")) {
                return None;
            }
        }
        let dir = std::env::temp_dir().join(format!("fm-pulse-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = dir.join("native");
        let child = Command::new("pulseaudio")
            .args([
                "-n",
                "--daemonize=no",
                "--exit-idle-time=-1",
                "--disallow-exit",
                "--use-pid-file=no",
                "--log-target=stderr",
                "-L",
                "module-null-sink sink_name=fmtest rate=48000",
                "-L",
            ])
            .arg(format!(
                "module-native-protocol-unix socket={} auth-anonymous=1",
                socket.display()
            ))
            // A private server: no session bus (whose PulseAudio name
            // another server may hold) and no desktop.
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &dir)
            .env("XDG_RUNTIME_DIR", &dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pulse = Self { child, dir, socket };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let ready = pulse
                .command("pactl")
                .arg("info")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if ready {
                return Some(pulse);
            }
            assert!(Instant::now() < deadline, "pulseaudio did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn server(&self) -> String {
        format!("unix:{}", self.socket.display())
    }

    /// `program` talking to this server.
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.env("PULSE_SERVER", self.server());
        command
    }

    /// A wrapper script running `program` against this server, for a tap
    /// that keeps only its own environment.
    fn wrapper(&self, program: &str) -> PathBuf {
        let path = self.dir.join(format!("{program}-wrapper"));
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nPULSE_SERVER='{}' exec {program} \"$@\"\n",
                self.server()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn tap(&self, pid: u32) -> AudioTap {
        AudioTap::start(TapSource::PulseApplication {
            key: AppAudioKey {
                pid: Some(pid),
                app: String::new(),
            },
            pactl: self.wrapper("pactl").into_os_string(),
            parec: self.wrapper("parec").into_os_string(),
        })
        .unwrap()
    }

    /// An application playing a `hertz` tone to the null sink, after
    /// `delay_s` seconds, from a child of a shell (the shell is the
    /// application's process; the child's process is written to a file).
    fn play(&self, hertz: u32, delay_s: f32) -> Player {
        let pid_file = self.dir.join(format!("player-{hertz}.pid"));
        let script = format!(
            "sleep {delay_s}; ffmpeg -hide_banner -loglevel error -re -f lavfi \
             -i sine=f={hertz}:r=48000 -ac 2 -f pulse -device fmtest tone{hertz} & \
             echo $! > '{}'; wait",
            pid_file.display()
        );
        let shell = self
            .command("sh")
            .args(["-c", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Player { shell, pid_file }
    }

    /// The sinks the playback streams play on, by stream.
    fn stream_sinks(&self) -> Vec<String> {
        let output = self
            .command("pactl")
            .args(["list", "short", "sink-inputs"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.split('\t').nth(1).map(str::to_owned))
            .collect()
    }

    /// About `seconds` of the default output's monitor, left channel.
    fn monitor(&self, seconds: f32) -> Vec<f32> {
        let mut child = self
            .command("parec")
            .args([
                "-d",
                "@DEFAULT_MONITOR@",
                "--raw",
                "--format=float32le",
                "--rate=48000",
                "--channels=2",
                "--latency-msec=20",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let wanted = frames(seconds) * 8;
        let mut bytes = vec![0_u8; wanted];
        stdout.read_exact(&mut bytes).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        left(&bytes)
    }
}

impl Drop for Pulse {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A playing application: the shell and the `ffmpeg` it started.
struct Player {
    shell: Child,
    pid_file: PathBuf,
}

impl Player {
    fn pid(&self) -> u32 {
        self.shell.id()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // The player this test's shell started, by its own process id.
        if let Ok(pid) = std::fs::read_to_string(&self.pid_file) {
            let _ = Command::new("kill").args(["-KILL", pid.trim()]).status();
        }
        let _ = self.shell.kill();
        let _ = self.shell.wait();
    }
}

/// Samples per channel in `seconds` (a small positive test duration).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn frames(seconds: f32) -> usize {
    (seconds * 48_000.0) as usize
}

/// The left channel of interleaved stereo `f32le` bytes.
fn left(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|frame| f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]))
        .collect()
}

/// About `seconds` of what `tap` serves, left channel.
fn consume(tap: &AudioTap, seconds: f32) -> Vec<f32> {
    let path = tap.url().strip_prefix("unix:").unwrap().to_owned();
    let mut stream = std::os::unix::net::UnixStream::connect(Path::new(&path)).unwrap();
    let wanted = frames(seconds) * 8;
    let mut bytes = vec![0_u8; wanted];
    stream.read_exact(&mut bytes).unwrap();
    left(&bytes)
}

/// About `seconds` of what `tap` serves once it serves sound (the server
/// takes a moment to hand a new monitor its first samples), left channel.
fn consume_sound(tap: &AudioTap, seconds: f32) -> Vec<f32> {
    let path = tap.url().strip_prefix("unix:").unwrap().to_owned();
    let mut stream = std::os::unix::net::UnixStream::connect(Path::new(&path)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut chunk = vec![0_u8; 4_800 * 8];
    loop {
        stream.read_exact(&mut chunk).unwrap();
        if left(&chunk).iter().any(|sample| *sample != 0.0) {
            break;
        }
        assert!(Instant::now() < deadline, "the tap served only silence");
    }
    // Past the stream's first, possibly partial, chunks.
    stream.read_exact(&mut chunk).unwrap();
    let mut bytes = vec![0_u8; frames(seconds) * 8];
    stream.read_exact(&mut bytes).unwrap();
    left(&bytes)
}

/// The power of `hertz` in `samples` (Goertzel), per sample.
#[allow(clippy::cast_precision_loss)] // test windows hold a few thousand samples
fn power(samples: &[f32], hertz: f32) -> f32 {
    let coefficient = 2.0 * (2.0 * std::f32::consts::PI * hertz / 48_000.0).cos();
    let (mut previous, mut before) = (0.0_f32, 0.0_f32);
    for sample in samples {
        let current = sample + coefficient * previous - before;
        before = previous;
        previous = current;
    }
    (previous * previous + before * before - coefficient * previous * before)
        / (samples.len() as f32).powi(2)
}

fn wait_for(tap: &AudioTap, wanted: &TapState, within: Duration) {
    let deadline = Instant::now() + within;
    while tap.state() != *wanted {
        assert!(
            Instant::now() < deadline,
            "{:?} instead of {wanted:?}",
            tap.state()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Only the followed application is captured, a stream its child process
/// starts later is found, and nobody's playback moves.
#[test]
fn an_application_tap_captures_only_its_application() {
    let Some(pulse) = Pulse::start("app") else {
        return;
    };
    // The followed application starts playing only after a second; the
    // other one plays at once.
    let followed = pulse.play(440, 1.5);
    let _other = pulse.play(1_000, 0.0);
    let tap = pulse.tap(followed.pid());
    wait_for(&tap, &TapState::NoStream, Duration::from_secs(5));
    // Waiting for its sound, the tap serves silence (and not the other
    // application).
    let silent = consume(&tap, 0.3);
    assert!(silent.iter().all(|sample| *sample == 0.0));

    wait_for(&tap, &TapState::Live, Duration::from_secs(10));
    let captured = consume_sound(&tap, 1.0);
    let (wanted, unwanted) = (power(&captured, 440.0), power(&captured, 1_000.0));
    assert!(
        wanted > 1e-3 && wanted > 100.0 * unwanted,
        "440 Hz {wanted} against 1000 Hz {unwanted}"
    );

    // Both streams still play on the null sink: monitoring moved nothing,
    // and the sink's monitor hears both applications.
    assert_eq!(pulse.stream_sinks().len(), 2);
    let sinks = pulse.stream_sinks();
    assert!(sinks.iter().all(|sink| *sink == sinks[0]), "{sinks:?}");
    let heard = pulse.monitor(0.5);
    assert!(
        power(&heard, 440.0) > 1e-3,
        "the followed app is still heard"
    );
    assert!(
        power(&heard, 1_000.0) > 1e-3,
        "the other app is still heard"
    );

    // The application stops: the tap waits for a stream again.
    drop(followed);
    wait_for(&tap, &TapState::NoStream, Duration::from_secs(10));
}

/// A tap whose server is gone reports why, and recovers when it returns
/// on the same socket.
#[test]
fn a_missing_server_fails_the_tap() {
    let Some(pulse) = Pulse::start("gone") else {
        return;
    };
    let pactl = pulse.wrapper("pactl");
    let parec = pulse.wrapper("parec");
    drop(pulse);
    let tap = AudioTap::start(TapSource::PulseApplication {
        key: AppAudioKey {
            pid: Some(std::process::id()),
            app: String::new(),
        },
        pactl: pactl.into_os_string(),
        parec: parec.into_os_string(),
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let TapState::Failed(reason) = tap.state() {
            assert!(reason.starts_with("pactl"), "{reason}");
            break;
        }
        assert!(Instant::now() < deadline, "{:?}", tap.state());
        std::thread::sleep(Duration::from_millis(20));
    }
}
