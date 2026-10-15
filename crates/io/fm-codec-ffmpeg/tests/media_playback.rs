//! IN-003 acceptance: frame-accurate playback of decoded video files and
//! growing files through `FilePlayer`.
//!
//! Fixtures are generated with `FFmpeg` lavfi sources. Every video frame
//! carries its own frame number as eight 16-pixel-wide black/white bit blocks
//! (`geq`), so frame accuracy is asserted from decoded pixels. Audio tracks are
//! sample-index ramps (`aevalsrc` with `n`), so sample accuracy is asserted from
//! decoded values. Tests skip when `ffmpeg`/`ffprobe` are unavailable unless
//! `FM_REQUIRE_FFMPEG=1`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::num::{NonZeroU32, NonZeroU128};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::{
    Adapter, Config, FilePlaybackConfig, FilePlaybackError, FilePlayer, GrowingFileConfig,
    PlaybackOutput, SourceRefresh, StreamSelector, ToolAvailability,
};
use fm_frame::{ClockDomainId, CpuVideoFrame};
use fm_playback::{
    AudioPlan, AudioSpeedPolicy, FrameIndex, MediaEndAction, MediaEvent, MediaPlayback,
    MediaSourceConfig, MediaState, PlaybackRate,
};
use fm_types::FrameRate;
use tempfile::tempdir;

const PROCESS_TIMEOUT: Duration = Duration::from_mins(1);
const WIDTH: usize = 128;
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
        eprintln!("skipping FFmpeg playback integration: tools unavailable: {capabilities:?}");
        None
    }
}

fn generate(args: &[&str], path: &Path) {
    let mut child = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args(args)
        .arg("-y")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("LC_ALL", "C")
        .spawn()
        .expect("spawn FFmpeg fixture generator");
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("poll FFmpeg generator") {
            assert!(
                status.success(),
                "FFmpeg fixture generation failed: {status}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "FFmpeg fixture generation timed out"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// Black/white bit blocks encoding the frame number (mod 256).
fn frame_number_video(seconds: u32) -> String {
    format!(
        "color=c=black:s=128x32:r=25:d={seconds},format=gray,\
         geq=lum='if(mod(floor(N/pow(2\\,floor(X/16)))\\,2)\\,235\\,16)'"
    )
}

/// 4 s, 25 fps, MPEG-2 with 12-frame GOPs and B-frames; track 1 is a mono
/// ramp `n / 192000`, track 2 a stereo `-n / 192000 | 0.5`, both 48 kHz PCM.
/// Matroska's millisecond timestamps bound audio placement to +-0.5 ms, so
/// sample-exact audio assertions use the NUT variant (per-stream time bases).
fn generate_mkv(path: &Path) {
    generate_multitrack(path, "matroska");
}

fn generate_nut(path: &Path) {
    generate_multitrack(path, "nut");
}

fn generate_multitrack(path: &Path, format: &str) {
    let video = frame_number_video(4);
    generate(
        &[
            "-f",
            "lavfi",
            "-i",
            &video,
            "-f",
            "lavfi",
            "-i",
            "aevalsrc='n/192000':s=48000:d=4",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc='-n/192000|0.5':s=48000:d=4",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map",
            "2:a",
            "-c:v",
            "mpeg2video",
            "-g",
            "12",
            "-bf",
            "2",
            "-q:v",
            "2",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "pcm_f32le",
            "-metadata:s:a:0",
            "language=eng",
            "-metadata:s:a:1",
            "language=spa",
            "-f",
            format,
        ],
        path,
    );
}

/// MPEG-TS (appendable; its demuxer snaps seeks forward to the next keyframe)
/// with MP2 audio.
fn generate_ts(path: &Path, seconds: u32, b_frames: &str) {
    let video = frame_number_video(seconds);
    let audio = format!("aevalsrc='n/192000':s=48000:d={seconds}");
    generate(
        &[
            "-f",
            "lavfi",
            "-i",
            &video,
            "-f",
            "lavfi",
            "-i",
            &audio,
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "mpeg2video",
            "-g",
            "12",
            "-bf",
            b_frames,
            "-q:v",
            "2",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "mp2",
            "-f",
            "mpegts",
        ],
        path,
    );
}

fn config(growing: Option<GrowingFileConfig>) -> FilePlaybackConfig {
    FilePlaybackConfig {
        growing,
        ..FilePlaybackConfig::new(
            ClockDomainId::new(NonZeroU128::new(7).unwrap()),
            FrameRate::new(25, 1).unwrap(),
        )
    }
}

fn frame_number(frame: &CpuVideoFrame) -> u64 {
    let bytes = frame.payload().planes()[0].bytes();
    (0..8).fold(0, |value, bit| {
        let x = bit * 16 + 8;
        let red = bytes[(16 * WIDTH + x) * 4];
        if red > 128 { value | (1 << bit) } else { value }
    })
}

/// Asserts the output pixels show exactly the frame the transport planned.
fn assert_exact(output: &PlaybackOutput) -> u64 {
    let planned = output.tick.frame.expect("a frame is planned").get();
    let video = output.video.as_ref().expect("a frame is decoded");
    assert!(!output.video_late, "frame {planned} was late");
    assert_eq!(
        frame_number(video),
        planned % 256,
        "pixels of frame {planned}"
    );
    planned
}

fn play(player: &mut FilePlayer, ticks: usize) -> Vec<u64> {
    (0..ticks)
        .map(|_| assert_exact(&player.tick().unwrap()))
        .collect()
}

#[test]
fn open_enumerates_audio_tracks_keyframes_and_rate() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("seek.mkv");
    generate_mkv(&path);
    let player = adapter.open_playback(&path, config(None)).unwrap();
    assert_eq!(player.frame_count(), 100);
    assert_eq!(player.frame_rate(), FrameRate::new(25, 1).unwrap());
    let tracks = player.audio_tracks();
    assert_eq!(
        tracks
            .iter()
            .map(|track| (track.stream_index, track.channels, track.sample_rate))
            .collect::<Vec<_>>(),
        [(1, 1, 48_000), (2, 2, 48_000)]
    );
    assert_eq!(tracks[0].language.as_deref(), Some("eng"));
    assert_eq!(tracks[1].language.as_deref(), Some("spa"));
    assert_eq!(player.controller().selected_audio(), Some(1));
    let keyframes = player.keyframes();
    assert_eq!(keyframes.first(), Some(&FrameIndex::new(0)));
    assert!(keyframes.len() > 2 && keyframes.len() < 50, "{keyframes:?}");
    assert_eq!(player.controller().state(), MediaState::Stopped);
}

fn assert_random_access_is_exact(player: &mut FilePlayer) {
    for target in [61, 13, 99, 0, 37, 12, 11, 50, 98, 1, 74] {
        let frame = player.decode_frame(FrameIndex::new(target)).unwrap();
        assert_eq!(frame_number(&frame), target, "seek to {target}");
        let report = player.telemetry().last_seek.unwrap();
        assert_eq!(report.target, FrameIndex::new(target));
        assert!(report.keyframe <= report.target);
        assert_eq!(
            report.decode_forward_frames,
            target - report.keyframe.get(),
            "{report:?}"
        );
    }
    assert!(player.decode_frame(FrameIndex::new(100)).is_err());
}

#[test]
fn keyframe_aware_seek_is_frame_exact_in_matroska_with_b_frames() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("seek.mkv");
    generate_mkv(&path);
    let mut player = adapter.open_playback(&path, config(None)).unwrap();
    assert_random_access_is_exact(&mut player);
}

#[test]
fn keyframe_aware_seek_is_frame_exact_in_nut() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("seek.nut");
    generate_nut(&path);
    let mut player = adapter.open_playback(&path, config(None)).unwrap();
    assert_eq!(player.frame_count(), 100);
    assert_random_access_is_exact(&mut player);
}

#[test]
fn keyframe_aware_seek_is_frame_exact_in_mpeg_ts_with_b_frames() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("seek.ts");
    generate_ts(&path, 4, "2");
    let mut player = adapter.open_playback(&path, config(None)).unwrap();
    assert_eq!(player.frame_count(), 100);
    assert_random_access_is_exact(&mut player);
}

#[test]
fn play_pause_resume_and_speed_changes_present_exact_frames() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("seek.mkv");
    generate_mkv(&path);
    let mut player = adapter.open_playback(&path, config(None)).unwrap();

    player.controller_mut().play();
    assert_eq!(play(&mut player, 15), (0..15).collect::<Vec<_>>());

    player.controller_mut().pause();
    let paused: Vec<_> = (0..3).map(|_| player.tick().unwrap()).collect();
    for output in &paused {
        assert_eq!(assert_exact(output), 15);
        assert_eq!(output.tick.state, MediaState::Paused);
    }
    assert!(paused[1].tick.repeated && paused[2].tick.repeated);
    let audio = paused[0].audio.as_ref().unwrap();
    assert!(audio.plane(0).unwrap().iter().all(|sample| *sample == 0.0));

    player.controller_mut().play();
    assert_eq!(play(&mut player, 3), [15, 16, 17]);

    player
        .controller_mut()
        .set_rate(PlaybackRate::new(2, 1).unwrap())
        .unwrap();
    let fast: Vec<_> = (0..4).map(|_| player.tick().unwrap()).collect();
    assert_eq!(
        fast.iter().map(assert_exact).collect::<Vec<_>>(),
        [18, 20, 22, 24]
    );
    assert_eq!(fast[1].tick.dropped_frames, 1);

    player
        .controller_mut()
        .set_rate(PlaybackRate::new(1, 2).unwrap())
        .unwrap();
    assert_eq!(play(&mut player, 4), [26, 26, 27, 27]);

    player
        .controller_mut()
        .set_rate(PlaybackRate::from_basis_points(40_000).unwrap())
        .unwrap();
    assert_eq!(play(&mut player, 3), [28, 32, 36]);
    assert_eq!(player.controller().position(), FrameIndex::new(40));
}

#[test]
fn trim_loop_and_end_triggers_on_a_decoded_file() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("seek.mkv");
    generate_mkv(&path);
    let mut player = adapter.open_playback(&path, config(None)).unwrap();

    let transport = player.controller_mut();
    transport
        .set_trim(FrameIndex::new(40), Some(FrameIndex::new(46)))
        .unwrap();
    transport.set_end_action(MediaEndAction::Loop);
    transport.play();
    let mut loops = Vec::new();
    let mut frames = Vec::new();
    for _ in 0..14 {
        let output = player.tick().unwrap();
        frames.push(assert_exact(&output));
        loops.extend(output.tick.events.iter().filter_map(|event| match event {
            MediaEvent::Looped { count } => Some(*count),
            _ => None,
        }));
    }
    assert_eq!(
        frames,
        [40, 41, 42, 43, 44, 45, 40, 41, 42, 43, 44, 45, 40, 41]
    );
    assert_eq!(loops, [1, 2]);

    // Hold the last frame with an early end trigger.
    let transport = player.controller_mut();
    transport.clear_trim();
    transport.set_end_action(MediaEndAction::HoldLastFrame);
    transport.set_end_trigger_lead(Some(2));
    transport.seek(FrameIndex::new(95)).unwrap();
    let mut events = Vec::new();
    let mut frames = Vec::new();
    for _ in 0..7 {
        let output = player.tick().unwrap();
        frames.push(assert_exact(&output));
        events.extend(output.tick.events);
    }
    assert_eq!(frames, [95, 96, 97, 98, 99, 99, 99]);
    assert_eq!(player.controller().state(), MediaState::Ended);
    assert!(events.contains(&MediaEvent::EndTrigger {
        remaining_frames: 2
    }));
    assert!(events.contains(&MediaEvent::Ended {
        action: MediaEndAction::HoldLastFrame
    }));

    // Stop rewinds to trim-in and cues it.
    let transport = player.controller_mut();
    transport
        .set_trim(FrameIndex::new(90), Some(FrameIndex::new(93)))
        .unwrap();
    transport.set_end_action(MediaEndAction::Stop);
    transport.play();
    assert_eq!(play(&mut player, 5), [90, 91, 92, 90, 90]);
    assert_eq!(player.controller().state(), MediaState::Stopped);
}

/// Expected ramp value of sample `index`; exact in `f32` below 2^24.
#[allow(clippy::cast_precision_loss)]
fn ramp(index: u32) -> f32 {
    index as f32 / RAMP
}

fn ramp_samples(output: &PlaybackOutput, channel: usize) -> Vec<f32> {
    output
        .audio
        .as_ref()
        .expect("audio block")
        .plane(channel)
        .expect("channel")
        .to_vec()
}

#[test]
fn audio_tracks_are_sample_exact_switchable_mid_play_and_follow_speed_policy() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("tracks.nut");
    generate_nut(&path);
    let mut player = adapter.open_playback(&path, config(None)).unwrap();
    player.controller_mut().seek(FrameIndex::new(50)).unwrap();
    player.controller_mut().play();

    // Track 1 (mono ramp) at frame 50 = sample 96 000, contiguous across ticks.
    for tick in 0..3_u32 {
        let output = player.tick().unwrap();
        let samples = ramp_samples(&output, 0);
        assert_eq!(samples.len(), 1_920);
        let first = 96_000 + tick * 1_920;
        for (offset, sample) in samples.iter().enumerate() {
            let expected = ramp(first + u32::try_from(offset).unwrap());
            assert!((sample - expected).abs() < 1e-6, "{sample} != {expected}");
        }
    }

    // Switch to track 2 (stereo) mid-play: continues at the exact sample.
    player.controller_mut().select_audio_track(Some(2)).unwrap();
    let output = player.tick().unwrap();
    assert_eq!(assert_exact(&output), 53);
    let block = output.audio.as_ref().unwrap();
    assert_eq!(block.channel_layout().channels().len(), 2);
    let left = ramp_samples(&output, 0);
    let right = ramp_samples(&output, 1);
    let first = 53 * 1_920_u32;
    assert!((left[0] + ramp(first)).abs() < 1e-6, "{}", left[0]);
    assert!((left[1_919] + ramp(first + 1_919)).abs() < 1e-6);
    assert!(right.iter().all(|sample| (sample - 0.5).abs() < 1e-6));
    assert!(output.tick.events.contains(&MediaEvent::AudioTrackChanged {
        from: Some(1),
        to: Some(2)
    }));

    // 2x with the default mute policy: silence.
    player.controller_mut().select_audio_track(Some(1)).unwrap();
    player
        .controller_mut()
        .set_rate(PlaybackRate::new(2, 1).unwrap())
        .unwrap();
    let output = player.tick().unwrap();
    assert_eq!(assert_exact(&output), 54);
    assert!(ramp_samples(&output, 0).iter().all(|sample| *sample == 0.0));

    // 2x varispeed: exactly the covered source span squeezed into one tick.
    player
        .controller_mut()
        .set_audio_policy(AudioSpeedPolicy::Resample);
    let output = player.tick().unwrap();
    assert_eq!(assert_exact(&output), 56);
    let samples = ramp_samples(&output, 0);
    assert_eq!(samples.len(), 1_920);
    let start = 56.0 * 1_920.0;
    assert!((samples[0] - start / RAMP).abs() < 1e-6);
    assert!((samples[1] - (start + 2.0) / RAMP).abs() < 1e-6);
    assert!((samples[1_919] - (start + 3_838.0) / RAMP).abs() < 1e-6);

    player.controller_mut().select_audio_track(None).unwrap();
    assert!(player.tick().unwrap().audio.is_none());
}

fn append(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

/// Splits at MPEG-TS packet boundaries, like a recorder flushing its buffer.
fn ts_cut(bytes: &[u8], numerator: usize, denominator: usize) -> usize {
    (bytes.len() * numerator / denominator) / 188 * 188
}

#[test]
fn growing_mpeg_ts_follows_the_live_edge_through_eof_and_more_data() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let full = directory.path().join("full.ts");
    generate_ts(&full, 6, "0");
    let bytes = fs::read(&full).unwrap();
    let live = directory.path().join("live.ts");
    let first_cut = ts_cut(&bytes, 3, 10);
    let second_cut = ts_cut(&bytes, 6, 10);
    fs::write(&live, &bytes[..first_cut]).unwrap();

    let mut player = adapter
        .open_playback(
            &live,
            config(Some(GrowingFileConfig {
                live_edge_offset_frames: 2,
                complete_after_idle_refreshes: None,
            })),
        )
        .unwrap();
    assert!(!player.controller().is_complete());
    let initially = player.frame_count();
    assert!(initially > 10 && initially < 150, "{initially}");
    player.controller_mut().set_end_trigger_lead(Some(5));
    player.controller_mut().play();

    let mut presented = Vec::new();
    let mut events = Vec::new();
    let mut appended = 0;
    for _ in 0..400 {
        let output = player.tick().unwrap();
        events.extend(output.tick.events.clone());
        if let Some(frame) = output.tick.frame {
            assert_exact(&output);
            if presented.last() != Some(&frame.get()) {
                presented.push(frame.get());
            }
        }
        if let Some(audio) = &output.audio {
            assert_eq!(audio.sample_count(), 1_920);
        }
        if output.tick.state == MediaState::Waiting {
            // EOF reached: the "recorder" writes more, then finishes.
            match appended {
                0 => append(&live, &bytes[first_cut..second_cut]),
                1 => append(&live, &bytes[second_cut..]),
                _ => {
                    assert_eq!(
                        player.complete_source().unwrap(),
                        SourceRefresh::Completed { frames: 150 }
                    );
                }
            }
            appended += 1;
        }
        if output.tick.state == MediaState::Ended {
            break;
        }
    }
    assert_eq!(presented, (0..150).collect::<Vec<_>>());
    assert_eq!(player.controller().state(), MediaState::Ended);
    let reached = events
        .iter()
        .filter(|event| matches!(event, MediaEvent::LiveEdgeReached { .. }))
        .count();
    let resumed = events
        .iter()
        .filter(|event| matches!(event, MediaEvent::LiveEdgeResumed { .. }))
        .count();
    assert!(reached >= 3 && resumed >= 2, "{reached} {resumed}");
    assert!(events.contains(&MediaEvent::SourceCompleted { frames: 150 }));
    // End triggers fire only once the end is final.
    let trigger = events
        .iter()
        .position(|event| matches!(event, MediaEvent::EndTrigger { .. }))
        .unwrap();
    let completed = events
        .iter()
        .position(|event| matches!(event, MediaEvent::SourceCompleted { .. }))
        .unwrap();
    assert!(trigger > completed);
}

#[test]
fn growing_source_completes_after_idle_refreshes() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let full = directory.path().join("full.ts");
    generate_ts(&full, 2, "0");
    let bytes = fs::read(&full).unwrap();
    let live = directory.path().join("live.ts");
    let cut = ts_cut(&bytes, 1, 2);
    fs::write(&live, &bytes[..cut]).unwrap();
    let mut player = adapter
        .open_playback(
            &live,
            config(Some(GrowingFileConfig {
                live_edge_offset_frames: 1,
                complete_after_idle_refreshes: NonZeroU32::new(2),
            })),
        )
        .unwrap();
    append(&live, &bytes[cut..]);
    assert!(matches!(
        player.refresh().unwrap(),
        SourceRefresh::Grew { .. }
    ));
    assert_eq!(player.refresh().unwrap(), SourceRefresh::Unchanged);
    assert_eq!(
        player.refresh().unwrap(),
        SourceRefresh::Completed { frames: 50 }
    );
    assert!(player.controller().is_complete());
    player.controller_mut().seek(FrameIndex::new(49)).unwrap();
    player.controller_mut().play();
    assert_eq!(play(&mut player, 2), [49, 49]);
}

#[test]
fn growing_source_refuses_truncation_replacement_and_rewrite() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let full = directory.path().join("full.ts");
    generate_ts(&full, 2, "0");
    let bytes = fs::read(&full).unwrap();
    let live = directory.path().join("live.ts");
    let growing = config(Some(GrowingFileConfig::default()));
    let cut = ts_cut(&bytes, 1, 2);

    fs::write(&live, &bytes[..cut]).unwrap();
    let mut player = adapter.open_playback(&live, growing).unwrap();
    OpenOptions::new()
        .write(true)
        .open(&live)
        .unwrap()
        .set_len(u64::try_from(cut - 188 * 10).unwrap())
        .unwrap();
    assert_eq!(
        player.refresh(),
        Err(FilePlaybackError::SourceTruncated {
            previous_bytes: u64::try_from(cut).unwrap(),
            current_bytes: u64::try_from(cut - 188 * 10).unwrap(),
        })
    );

    // Log rotation: a new file is renamed over the followed path.
    fs::write(&live, &bytes[..cut]).unwrap();
    let mut player = adapter.open_playback(&live, growing).unwrap();
    let rotated = directory.path().join("rotated.ts");
    fs::write(&rotated, &bytes).unwrap();
    fs::rename(&rotated, &live).unwrap();
    assert_eq!(player.refresh(), Err(FilePlaybackError::SourceReplaced));
    fs::remove_file(&live).unwrap();
    assert_eq!(player.refresh(), Err(FilePlaybackError::SourceReplaced));

    // In-place rewrite of already-read bytes.
    fs::write(&live, &bytes[..cut]).unwrap();
    let mut player = adapter.open_playback(&live, growing).unwrap();
    let mut rewritten = bytes[..cut].to_vec();
    rewritten[200] ^= 0xff;
    rewritten.extend_from_slice(&bytes[cut..]);
    OpenOptions::new()
        .write(true)
        .open(&live)
        .unwrap()
        .write_all(&rewritten)
        .unwrap();
    assert_eq!(player.refresh(), Err(FilePlaybackError::SourceRewritten));

    // A complete (non-growing) source must not change at all.
    fs::write(&live, &bytes).unwrap();
    let mut player = adapter.open_playback(&live, config(None)).unwrap();
    append(&live, &[0x47; 188]);
    assert_eq!(player.refresh(), Err(FilePlaybackError::SourceChanged));
    player.controller_mut().play();
    assert_eq!(player.tick(), Err(FilePlaybackError::SourceChanged));
}

#[test]
fn explicit_stream_selection_and_missing_video_are_typed() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("seek.mkv");
    generate_mkv(&path);
    let player = adapter
        .open_playback(
            &path,
            FilePlaybackConfig {
                audio: Some(StreamSelector::Index(2)),
                ..config(None)
            },
        )
        .unwrap();
    assert_eq!(player.controller().selected_audio(), Some(2));
    assert!(
        adapter
            .open_playback(
                &path,
                FilePlaybackConfig {
                    audio: Some(StreamSelector::Index(0)),
                    ..config(None)
                },
            )
            .is_err()
    );
    let audio_only = directory.path().join("audio.wav");
    generate(
        &["-f", "lavfi", "-i", "aevalsrc='n/192000':s=48000:d=1"],
        &audio_only,
    );
    assert!(adapter.open_playback(&audio_only, config(None)).is_err());
}

/// An authority that owns the transport elsewhere (the engine) hands each of
/// its ticks to `present`, which decodes exactly what the tick plans; the
/// player's own transport stays put. `prefetch` decodes the next window
/// ahead, so forward playback crosses window boundaries without a new tool
/// run.
#[test]
fn present_decodes_an_external_transports_ticks_and_prefetch_reads_ahead() {
    let _guard = guard();
    let Some(adapter) = require_ffmpeg() else {
        return;
    };
    let directory = tempdir().unwrap();
    let path = directory.path().join("external.mkv");
    generate_mkv(&path);
    let mut player = adapter.open_playback(&path, config(None)).unwrap();
    let mut transport = MediaPlayback::new(MediaSourceConfig {
        source_rate: player.frame_rate(),
        output_rate: FrameRate::new(25, 1).unwrap(),
        available_frames: player.frame_count(),
        complete: true,
        audio_tracks: player.audio_tracks().to_vec(),
        selected_audio: Some(2),
    })
    .unwrap();
    assert!(!player.prefetch().unwrap(), "nothing presented yet");
    transport.play();
    let first = player.present(transport.tick()).unwrap();
    assert_eq!(assert_exact(&first), 0);
    let audio = first.audio.as_ref().unwrap();
    assert_eq!((audio.sample_count(), audio.planes().len()), (1_920, 2));
    let windows = player.telemetry().video_windows;
    assert!(player.prefetch().unwrap());
    assert!(
        !player.prefetch().unwrap(),
        "the next window is already decoded"
    );
    assert_eq!(player.telemetry().video_windows, windows + 1);
    // Frames 1..=23 come from the presented and the prefetched windows.
    for expected in 1..24 {
        assert_eq!(
            assert_exact(&player.present(transport.tick()).unwrap()),
            expected
        );
    }
    assert_eq!(player.telemetry().video_windows, windows + 1);
    // The player's own transport never moved.
    assert_eq!(player.controller().position(), FrameIndex::new(0));
    assert_eq!(player.controller().state(), MediaState::Stopped);

    transport.seek(FrameIndex::new(70)).unwrap();
    let seeked = player.present(transport.tick()).unwrap();
    assert_eq!(assert_exact(&seeked), 70);
    assert!(matches!(
        seeked.tick.audio,
        AudioPlan::Source { track: 2, .. }
    ));
    transport.select_audio_track(Some(9)).unwrap_err();
    let mut unknown = transport.tick();
    unknown.audio = AudioPlan::Silence {
        track: 9,
        output_samples: 1_920,
    };
    assert!(matches!(
        player.present(unknown),
        Err(FilePlaybackError::Control(_))
    ));
}
