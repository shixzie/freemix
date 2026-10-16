//! The `ffmpeg` acquisition path without hardware: the `lavfi` demuxer
//! stands in for `v4l2` and `alsa`, so the supervisor drives a real child
//! through the same device-input arguments (deinterlace, scale, rate, and
//! a separate audio input), goes live with audio, and falls back to video
//! only when the audio input cannot open.
//!
//! Skips when `ffmpeg` is unavailable unless `FM_REQUIRE_FFMPEG=1`.

use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fm_io_linux::probe::ProbedDevice;
use fm_io_linux::{CaptureSupervisor, FfmpegAcquirer, PulledPicture, SupervisorTiming};
use fm_types::FrameRate;
use fm_types::capture::{
    CaptureAudio, CaptureDevice, CaptureFormat, CaptureSettings, CaptureSignalState, CaptureTarget,
};

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

fn lavfi() -> FfmpegAcquirer {
    FfmpegAcquirer {
        demuxer: "lavfi".into(),
        audio_demuxer: "lavfi".into(),
        format_options: false,
        ..FfmpegAcquirer::v4l2()
    }
}

fn device(audio: Option<&str>, format: &str) -> Vec<ProbedDevice> {
    vec![ProbedDevice {
        info: CaptureDevice {
            stable_key: "v4l2:lavfi#Test Pattern".into(),
            name: "Test Pattern".into(),
            path: "testsrc2=size=320x240:rate=30,realtime".into(),
            driver: "lavfi".into(),
            bus: "lavfi".into(),
            audio: audio.map(str::to_owned),
            formats: vec![CaptureFormat::from_token(format).unwrap()],
        },
        no_signal: false,
    }]
}

/// Runs the supervisor at the program rate until it has pulled `pictures`
/// new pictures (or ten seconds pass), returning the audio blocks heard.
fn run(
    supervisor: &mut CaptureSupervisor,
    acquirer: &FfmpegAcquirer,
    devices: &[ProbedDevice],
    pictures: usize,
) -> (usize, Vec<Vec<Vec<f32>>>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = 0;
    let mut audio = Vec::new();
    while seen < pictures && Instant::now() < deadline {
        let now = Instant::now();
        supervisor.update(acquirer, devices, now);
        let pull = supervisor.pull(now);
        if let PulledPicture::New(picture) = pull.picture {
            assert_eq!(picture.len(), 64 * 36 * 4);
            seen += 1;
        }
        if let Some(block) = pull.audio {
            audio.push(block);
        }
        std::thread::sleep(Duration::from_millis(33));
    }
    (seen, audio)
}

fn supervisor(audio: CaptureAudio) -> CaptureSupervisor {
    CaptureSupervisor::new(
        "v4l2:lavfi#Test Pattern".into(),
        CaptureSettings {
            audio,
            ..CaptureSettings::default()
        },
        CaptureTarget {
            width: 64,
            height: 36,
            rate: FrameRate::new(30, 1).unwrap(),
        },
        SupervisorTiming::default(),
    )
}

#[test]
fn a_device_child_delivers_deinterlaced_scaled_pictures_and_paired_audio() {
    if !ffmpeg_available() {
        return;
    }
    let _serial = serialize();
    let acquirer = lavfi();
    let devices = device(
        Some("sine=frequency=440:sample_rate=48000,arealtime"),
        "320x240@30/1:yuyv:tff",
    );
    let mut supervisor = supervisor(CaptureAudio::Paired);
    let (pictures, audio) = run(&mut supervisor, &acquirer, &devices, 20);
    assert_eq!(pictures, 20, "{:?}", supervisor.status());
    let status = supervisor.status();
    assert_eq!(status.state, CaptureSignalState::Live);
    assert!(status.audio, "{status:?}");
    // Each frame's audio is exactly one program frame of 48 kHz stereo.
    let block = audio.last().expect("audio blocks");
    assert_eq!(block.len(), 2);
    assert_eq!(block[0].len(), 1_600);
    assert!(
        audio
            .iter()
            .flatten()
            .flatten()
            .any(|sample| sample.abs() > 0.01),
        "the tone is heard"
    );
}

#[test]
fn an_audio_input_that_cannot_open_falls_back_to_pictures_only() {
    if !ffmpeg_available() {
        return;
    }
    let _serial = serialize();
    let acquirer = Arc::new(lavfi());
    let devices = device(None, "320x240@30/1:mjpeg:p");
    let mut supervisor = supervisor(CaptureAudio::Alsa("nosuchfilter_xyz".into()));
    let (pictures, _) = run(&mut supervisor, &acquirer, &devices, 5);
    assert_eq!(pictures, 5, "{:?}", supervisor.status());
    assert_eq!(supervisor.status().state, CaptureSignalState::Live);
    assert!(!supervisor.status().audio);
}
