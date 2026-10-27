//! IN-005 / IN-003 acceptance: audio-only media files (music beds and
//! stings) play through `FilePlayer` like video files.
//!
//! A file without a video stream opens only when the configuration names
//! the size of its picture; its frame timeline is synthesized at the output
//! rate from the container duration, and every frame shows one shared
//! transparent-black picture. Transport (seek, loop, stop, end trigger) and
//! sample-exact audio placement behave as for video. Fixtures are generated
//! with `FFmpeg` lavfi sources (`sine`, and an `aevalsrc` sample-index ramp
//! so placement is asserted from decoded values). Tests skip when
//! `ffmpeg`/`ffprobe` are unavailable unless `FM_REQUIRE_FFMPEG=1`.

use std::num::NonZeroU128;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use fm_codec_ffmpeg::{
    Adapter, Config, Error, FilePlaybackConfig, FilePlaybackError, GrowingFileConfig, StreamKind,
    ToolAvailability,
};
use fm_frame::{AlphaMode, AudioBlock, ClockDomainId, VideoDimensions};
use fm_playback::{AudioPlan, FrameIndex, MediaEndAction, MediaEvent, MediaState};
use fm_types::FrameRate;
use tempfile::tempdir;

const RAMP: f32 = 192_000.0;
static FFMPEG_TEST_LOCK: Mutex<()> = Mutex::new(());

fn guard() -> MutexGuard<'static, ()> {
    FFMPEG_TEST_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn require_ffmpeg() -> Option<Adapter> {
    let adapter = Adapter::new(Config {
        limits: fm_codec_ffmpeg::Limits {
            discovery_timeout: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(30),
            frame_metadata_timeout: Duration::from_mins(1),
            decode_timeout: Duration::from_mins(1),
            ..fm_codec_ffmpeg::Limits::default()
        },
        ..Config::default()
    })
    .expect("valid adapter configuration");
    let capabilities = adapter.capabilities();
    let available = matches!(capabilities.ffmpeg, ToolAvailability::Available { .. })
        && matches!(capabilities.ffprobe, ToolAvailability::Available { .. });
    if available {
        Some(adapter)
    } else if std::env::var("FM_REQUIRE_FFMPEG").as_deref() == Ok("1") {
        panic!("FM_REQUIRE_FFMPEG=1 but FFmpeg tools are unavailable: {capabilities:?}");
    } else {
        eprintln!("skipping FFmpeg audio-only integration: tools unavailable: {capabilities:?}");
        None
    }
}

fn generate(args: &[&str], path: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args(args)
        .arg("-y")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("LC_ALL", "C")
        .status()
        .expect("spawn FFmpeg fixture generator");
    assert!(
        status.success(),
        "FFmpeg fixture generation failed: {status}"
    );
}

/// One second of a mono 48 kHz sample-index ramp (`n / 192000`).
fn generate_ramp_wav(path: &Path) {
    generate(
        &[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc='n/192000':s=48000:d=1",
            "-c:a",
            "pcm_f32le",
        ],
        path,
    );
}

/// `seconds` of a 440 Hz stereo sine at 48 kHz, encoded as `path`'s
/// extension says.
fn generate_sine(path: &Path, seconds: &str) {
    generate(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:sample_rate=48000:duration={seconds}"),
            "-ac",
            "2",
        ],
        path,
    );
}

fn rate() -> FrameRate {
    FrameRate::new(25, 1).unwrap()
}

fn config() -> FilePlaybackConfig {
    FilePlaybackConfig {
        audio_only_picture: VideoDimensions::new(64, 36),
        ..FilePlaybackConfig::new(ClockDomainId::new(NonZeroU128::MIN), rate())
    }
}

fn mono(block: &AudioBlock) -> &[f32] {
    block.plane(0).unwrap()
}

fn peak(block: &AudioBlock) -> f32 {
    block
        .planes()
        .iter()
        .flatten()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
}

fn assert_ramp(block: &AudioBlock, first: u64) {
    for (offset, sample) in mono(block).iter().enumerate() {
        #[allow(clippy::cast_precision_loss)] // sample indexes below 2^24
        let expected = (first + offset as u64) as f32 / RAMP;
        assert!(
            (sample - expected).abs() < 1e-6,
            "sample {} is {sample}, expected {expected}",
            first + offset as u64
        );
    }
}

#[test]
fn audio_only_files_are_refused_without_a_picture_size_and_when_growing() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("bed.wav");
    generate_ramp_wav(&path);

    let probe = adapter.probe_local(&path).unwrap();
    assert!(
        probe
            .streams
            .iter()
            .all(|stream| stream.kind == StreamKind::Audio)
    );

    let plain = FilePlaybackConfig::new(ClockDomainId::new(NonZeroU128::MIN), rate());
    assert!(matches!(
        adapter.open_playback(&path, plain),
        Err(FilePlaybackError::Ffmpeg(Error::MissingStream))
    ));
    let growing = FilePlaybackConfig {
        growing: Some(GrowingFileConfig::default()),
        ..config()
    };
    assert!(matches!(
        adapter.open_playback(&path, growing),
        Err(FilePlaybackError::Ffmpeg(Error::MissingStream))
    ));
}

#[test]
fn an_audio_only_file_presents_one_transparent_picture_and_sample_exact_audio() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("bed.wav");
    generate_ramp_wav(&path);

    let mut player = adapter.open_playback(&path, config()).unwrap();
    assert!(player.is_audio_only());
    assert_eq!(player.frame_rate(), rate());
    // One second at 25 fps.
    assert_eq!(player.frame_count(), 25);
    assert_eq!(player.audio_tracks().len(), 1);
    assert_eq!(player.audio_tracks()[0].sample_rate, 48_000);
    assert_eq!(player.controller().selected_audio(), Some(0));

    // The picture: transparent black at the configured size, straight
    // alpha, and the same shared frame on every tick.
    let cue = player.decode_frame(FrameIndex::new(0)).unwrap();
    let size = cue.payload().dimensions();
    assert_eq!((size.width(), size.height()), (64, 36));
    assert!(
        cue.payload()
            .planes()
            .iter()
            .all(|plane| plane.bytes().iter().all(|&byte| byte == 0))
    );
    assert_eq!(
        cue.metadata()
            .and_then(fm_frame::VideoFrameMetadata::alpha_mode),
        Some(AlphaMode::Straight)
    );
    assert!(player.decode_frame(FrameIndex::new(25)).is_err());

    player.controller_mut().play();
    let mut first = None;
    for tick in 0..3_u64 {
        let output = player.tick().unwrap();
        assert_eq!(output.tick.frame, Some(FrameIndex::new(tick)));
        let picture = output.video.unwrap();
        let first = first.get_or_insert_with(|| Arc::clone(&picture));
        assert!(Arc::ptr_eq(first, &picture));
        let audio = output.audio.unwrap();
        assert_eq!(audio.sample_count(), 1920);
        assert_ramp(&audio, tick * 1920);
    }
    // No video window is ever decoded.
    assert_eq!(player.telemetry().video_windows, 0);
    assert!(!player.prefetch().unwrap());

    // A seek lands audio on the exact sample.
    player.controller_mut().seek(FrameIndex::new(10)).unwrap();
    let output = player.tick().unwrap();
    assert_eq!(output.tick.frame, Some(FrameIndex::new(10)));
    assert_ramp(&output.audio.unwrap(), 10 * 1920);
}

#[test]
fn an_audio_only_file_loops_stops_and_fires_its_end_trigger() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("sting.wav");
    generate_ramp_wav(&path);

    // Loop: after the last frame, the transport wraps to frame zero and
    // audio wraps to sample zero.
    let mut player = adapter.open_playback(&path, config()).unwrap();
    player.controller_mut().set_end_action(MediaEndAction::Loop);
    player.controller_mut().set_end_trigger_lead(Some(5));
    player.controller_mut().seek(FrameIndex::new(18)).unwrap();
    player.controller_mut().play();
    let mut triggered = false;
    let mut looped = false;
    let mut frames = Vec::new();
    for _ in 0..10 {
        let output = player.tick().unwrap();
        triggered |= output
            .tick
            .events
            .iter()
            .any(|event| matches!(event, MediaEvent::EndTrigger { .. }));
        looped |= output
            .tick
            .events
            .contains(&MediaEvent::Looped { count: 1 });
        let frame = output.tick.frame.unwrap().get();
        frames.push(frame);
        assert_ramp(&output.audio.unwrap(), frame * 1920);
    }
    assert!(triggered, "the end trigger did not fire");
    assert!(looped, "the transport did not loop");
    assert_eq!(frames, [18, 19, 20, 21, 22, 23, 24, 0, 1, 2]);

    // Stop: the transport rewinds and stops, and audio falls silent.
    let mut player = adapter.open_playback(&path, config()).unwrap();
    player.controller_mut().set_end_action(MediaEndAction::Stop);
    player.controller_mut().seek(FrameIndex::new(22)).unwrap();
    player.controller_mut().play();
    let mut ended = false;
    for _ in 0..6 {
        let output = player.tick().unwrap();
        ended |= output.tick.events.contains(&MediaEvent::Ended {
            action: MediaEndAction::Stop,
        });
    }
    assert!(ended, "the transport did not end");
    assert_eq!(player.controller().state(), MediaState::Stopped);
    let output = player.tick().unwrap();
    assert!(!matches!(output.tick.audio, AudioPlan::Source { .. }));
}

#[test]
fn common_audio_formats_play_as_audio_only_media() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    for name in ["bed.wav", "bed.mp3", "bed.m4a", "bed.flac", "bed.ogg"] {
        let path = directory.path().join(name);
        generate_sine(&path, "2");
        let mut player = adapter
            .open_playback(&path, config())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(player.is_audio_only(), "{name}");
        // Two seconds at 25 fps; MP3 and AAC add a little encoder padding.
        let frames = player.frame_count();
        assert!((50..=53).contains(&frames), "{name}: {frames} frames");
        player.controller_mut().seek(FrameIndex::new(20)).unwrap();
        player.controller_mut().play();
        for _ in 0..3 {
            let output = player.tick().unwrap();
            let audio = output.audio.unwrap();
            assert_eq!(audio.planes().len(), 2, "{name}");
            // The mono sine (amplitude 1/8) upmixed to stereo at -3 dB.
            let level = peak(&audio);
            assert!((0.07..0.11).contains(&level), "{name}: peak {level}");
        }
    }
}

/// A fragmented MP4 like the recorder writes opens its AAC stream with the
/// encoder priming packet at 0, ahead of the 80 ms its B-frame video shifts
/// every other packet: the decode skips that packet and starts at the
/// second, whose timestamp its first block carries.
#[test]
fn a_leading_priming_packet_ahead_of_a_gap_is_skipped() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("fragmented.mp4");
    generate(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=s=128x72:r=25:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=48000:d=2",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "25",
            "-c:a",
            "aac",
            "-movflags",
            "+empty_moov+default_base_moof+frag_keyframe",
            "-f",
            "mp4",
        ],
        &path,
    );
    let decoded = adapter
        .decode_local_up_to(
            &path,
            fm_codec_ffmpeg::DecodeRequest {
                clock_domain: ClockDomainId::new(NonZeroU128::new(1).unwrap()),
                video: None,
                audio: Some(fm_codec_ffmpeg::SequenceRequest {
                    selector: fm_codec_ffmpeg::StreamSelector::Best,
                    count: core::num::NonZeroU32::new(8).unwrap(),
                }),
            },
        )
        .expect("the stream decodes from its second packet");
    assert_eq!(decoded.audio.len(), 8);
    let first = decoded.audio[0].timing().original_timestamp();
    // The second packet sits at the video's 80 ms shift, not at 1024.
    assert_eq!(first.timestamp().ticks(), 3_840);
    assert!(decoded.audio.iter().all(|block| peak(block) > 0.05));
}
