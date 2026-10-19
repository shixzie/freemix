#![cfg(unix)]

//! Audio-only system audio receiver coverage (AU-011).
//!
//! Each case reads a real ALSA capture device through the real `ffmpeg`
//! with a [`NetworkReceiver`] built by [`NetworkInputConfig::audio_device`],
//! pulled at the program rate like the session loop pulls it: the ALSA
//! `null` device (whose capture content is host-defined, so only its
//! pacing is checked) and a PCM name ALSA does not know. The tone case runs
//! a wrapper `ffmpeg` that replaces the device `freemix_tone` with a 1 kHz
//! `lavfi` tone paced in real time, so the receiver's level is measured
//! exactly.
//!
//! The cases skip when `ffmpeg` or ALSA is unavailable, unless
//! `FM_REQUIRE_FFMPEG=1` or `FM_REQUIRE_ALSA=1` turns that into a failure.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::Executable;
use fm_codec_ffmpeg::network_input::{NetworkInputConfig, NetworkReceiver, Picture};
use fm_types::FrameRate;
use fm_types::network_stream::{
    GiveUp, NetworkStreamSettings, NetworkStreamState, NetworkStreamStatus, ReconnectPolicy,
};

const FPS: u32 = 25;
const FRAME_PERIOD: Duration = Duration::from_millis(40);

static SERIAL: Mutex<()> = Mutex::new(());

fn serialize() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn ffmpeg_available() -> bool {
    let available = Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    assert!(
        available || std::env::var("FM_REQUIRE_FFMPEG").as_deref() != Ok("1"),
        "FM_REQUIRE_FFMPEG=1 but ffmpeg is unavailable"
    );
    available
}

/// Whether the real `ffmpeg` can capture from the ALSA `null` device.
fn alsa_available() -> bool {
    let available = ffmpeg_available()
        && Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
            .args(["-f", "alsa", "-i", "null", "-t", "0.1", "-f", "null", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
    assert!(
        available || std::env::var("FM_REQUIRE_ALSA").as_deref() != Ok("1"),
        "FM_REQUIRE_ALSA=1 but ffmpeg cannot capture from ALSA"
    );
    available
}

fn real_ffmpeg() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("ffmpeg"))
        .find(|candidate| candidate.is_file())
        .expect("ffmpeg is on PATH")
}

/// An `ffmpeg` wrapper in `directory` that plays the device
/// `freemix_tone` as a 1 kHz tone at -6 dBFS paced in real time
/// (`arealtime`), and passes every other invocation to the real `ffmpeg`.
fn tone_wrapper(directory: &Path) -> PathBuf {
    let wrapper = directory.join("ffmpeg");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n\
             tone=''\n\
             for arg do [ \"$arg\" = freemix_tone ] && tone=1; done\n\
             if [ -n \"$tone\" ]; then\n\
             for arg do\n\
             shift\n\
             case \"$arg\" in\n\
             alsa) set -- \"$@\" lavfi ;;\n\
             freemix_tone) set -- \"$@\" 'sine=frequency=1000:sample_rate=48000,volume=4,pan=stereo|c0=c0|c1=c0,arealtime' ;;\n\
             *) set -- \"$@\" \"$arg\" ;;\n\
             esac\n\
             done\n\
             fi\n\
             exec '{}' \"$@\"\n",
            real_ffmpeg().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    wrapper
}

fn settings() -> NetworkStreamSettings {
    NetworkStreamSettings {
        jitter_buffer_ms: 120,
        reconnect: ReconnectPolicy {
            backoff_min_ms: 200,
            backoff_max_ms: 400,
            give_up: GiveUp::Never,
        },
        timeout_ms: 2_000,
        ..NetworkStreamSettings::default()
    }
}

fn receiver(device: &str, ffmpeg: Option<PathBuf>) -> NetworkReceiver {
    let mut config = NetworkInputConfig::audio_device(
        "alsa",
        device,
        settings(),
        FrameRate::new(FPS, 1).unwrap(),
    );
    if let Some(ffmpeg) = ffmpeg {
        config.ffmpeg = Executable::Explicit(ffmpeg);
    }
    NetworkReceiver::start(config).expect("start the receiver")
}

#[derive(Default)]
struct Observed {
    pictures: usize,
    loud_frames: usize,
    peak: f32,
}

fn pull_until(
    receiver: &NetworkReceiver,
    limit: Duration,
    observed: &mut Observed,
    mut done: impl FnMut(&NetworkStreamStatus, &Observed) -> bool,
) -> bool {
    let deadline = Instant::now() + limit;
    let mut next = Instant::now();
    while Instant::now() < deadline {
        let frame = receiver.pull();
        if let Picture::New(bytes) = &frame.picture {
            assert_eq!(bytes.len(), 2 * 2 * 4, "a 2x2 pacing picture");
            observed.pictures += 1;
        }
        assert_eq!(frame.audio.len(), 2);
        assert!(frame.audio.iter().all(|plane| plane.len() == 1_920));
        let peak = frame.audio[0]
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0, f32::max);
        observed.peak = observed.peak.max(peak);
        if peak > 0.1 {
            observed.loud_frames += 1;
        }
        if done(&receiver.status(), observed) {
            return true;
        }
        next += FRAME_PERIOD;
        thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    false
}

#[test]
fn the_alsa_null_device_goes_live_with_paced_pictures() {
    if !alsa_available() {
        eprintln!("system audio receiver skipped: ALSA capture unavailable");
        return;
    }
    let _serial = serialize();
    let receiver = receiver("null", None);
    let mut observed = Observed::default();
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(20),
            &mut observed,
            |status, _| { status.state == NetworkStreamState::Live }
        ),
        "never went live: {:?}",
        receiver.status()
    );
    let pictures = observed.pictures;
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(10),
            &mut observed,
            |_, seen| { seen.pictures >= pictures + 25 }
        ),
        "live without paced pictures: {:?}",
        receiver.status()
    );
    assert!(receiver.audio_active());
}

#[test]
fn a_tone_device_is_heard_at_its_level() {
    if !alsa_available() {
        eprintln!("system audio receiver skipped: ALSA capture unavailable");
        return;
    }
    let _serial = serialize();
    let directory = tempfile::tempdir().unwrap();
    let wrapper = tone_wrapper(directory.path());
    let receiver = receiver("freemix_tone", Some(wrapper));
    let mut observed = Observed::default();
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(20),
            &mut observed,
            |_, seen| { seen.loud_frames >= 10 }
        ),
        "the tone was never heard: {:?} (peak {})",
        receiver.status(),
        observed.peak
    );
    // A -6 dBFS tone: about half of full scale, never clipped.
    assert!(
        (0.4..=0.6).contains(&observed.peak),
        "tone peak {}",
        observed.peak
    );
    assert_eq!(receiver.status().state, NetworkStreamState::Live);
}

#[test]
fn a_missing_device_reopens_with_its_error_and_never_drops_its_audio() {
    if !alsa_available() {
        eprintln!("system audio receiver skipped: ALSA capture unavailable");
        return;
    }
    let _serial = serialize();
    let receiver = receiver("freemix_no_such_pcm", None);
    let mut observed = Observed::default();
    assert!(
        pull_until(
            &receiver,
            Duration::from_secs(20),
            &mut observed,
            |status, _| {
                matches!(status.state, NetworkStreamState::Reconnecting { attempt, .. } if attempt >= 2)
            }
        ),
        "a missing device did not keep reopening: {:?}",
        receiver.status()
    );
    let status = receiver.status();
    assert!(status.reconnects >= 2, "{status:?}");
    assert!(
        status
            .last_error
            .as_deref()
            .is_some_and(|error| !error.is_empty()),
        "{status:?}"
    );
    assert_eq!(observed.pictures, 0);
    assert!(receiver.audio_active(), "audio is never dropped");
}
