use std::num::NonZeroU128;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::record::{
    CleanupStatus, EnqueueRejection, OutputFinalization, PairedFrame, RecordConfig, RecordFormat,
    Recorder, StartErrorKind, StopOutcome,
};
use fm_frame::{
    AudioBlock, ChannelLayout, ClockDomainId, MediaTimestamp, MediaTiming, NormalizedDuration,
    NormalizedTimestamp, OriginalTimestamp, SampleRate, SequenceNumber, TimeBase,
};
use fm_types::FrameRate;
use tempfile::tempdir;

const FRAME_COUNT: u64 = 90;

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
    available
}

fn format() -> RecordFormat {
    RecordFormat::new(
        64,
        48,
        FrameRate::new(30, 1).unwrap(),
        SampleRate::new(48_000).unwrap(),
        ChannelLayout::stereo(),
        SequenceNumber::new(100),
    )
    .unwrap()
}

fn frame(format: &RecordFormat, offset: u64) -> PairedFrame {
    let sequence = SequenceNumber::new(format.first_sequence().get() + offset);
    let absolute_start_sample = sequence.get() * 1_600;
    let absolute_start_nanos = sequence.get() * 1_000_000_000 / 30;
    let absolute_end_nanos = (sequence.get() + 1) * 1_000_000_000 / 30;
    let timing = MediaTiming::new(
        OriginalTimestamp::new(
            MediaTimestamp::new(i64::try_from(absolute_start_sample).unwrap()),
            TimeBase::new(1, 48_000).unwrap(),
        ),
        NormalizedTimestamp::from_nanos(i64::try_from(absolute_start_nanos).unwrap()),
        NormalizedDuration::from_nanos(absolute_end_nanos - absolute_start_nanos).unwrap(),
        ClockDomainId::new(NonZeroU128::new(7).unwrap()),
        sequence,
    )
    .unwrap();
    let sample_count = 1_600;
    let left = vec![0.1; sample_count];
    let right = vec![-0.1; sample_count];
    let audio = AudioBlock::new(
        timing,
        format.sample_rate(),
        format.channel_layout().clone(),
        vec![left, right],
    )
    .unwrap();
    let mut rgba = vec![0_u8; format.rgba_bytes_per_frame()];
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[
            u8::try_from(offset % 255).unwrap(),
            80,
            u8::try_from((offset * 3) % 255).unwrap(),
            255,
        ]);
    }
    PairedFrame::new(format, sequence, rgba, audio).unwrap()
}

fn enqueue_all(recorder: &mut Recorder, format: &RecordFormat) {
    for offset in 0..FRAME_COUNT {
        let mut pending = frame(format, offset);
        loop {
            match recorder.enqueue(pending) {
                Ok(()) => break,
                Err(error) if error.reason == EnqueueRejection::QueueFull => {
                    pending = error.into_frame();
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!(
                    "unexpected enqueue rejection: {:?}; telemetry: {:?}",
                    error.reason,
                    recorder.telemetry()
                ),
            }
        }
    }
}

fn probe(path: &std::path::Path) -> serde_json::Value {
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_frames",
            "-count_packets",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        probe.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&probe.stderr)
    );
    serde_json::from_slice(&probe.stdout).unwrap()
}

fn numeric_string(value: &serde_json::Value, field: &str) -> f64 {
    value[field].as_str().unwrap().parse().unwrap()
}

fn count(value: &serde_json::Value, field: &str) -> u64 {
    value[field].as_str().unwrap().parse().unwrap()
}

fn assert_probe_and_decode(path: &std::path::Path, minimum_video_frames: u64) {
    let probe = probe(path);
    let minimum_seconds = f64::from(u32::try_from(minimum_video_frames).unwrap()) / 30.0;
    let streams = probe["streams"].as_array().unwrap();
    let video = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .unwrap();
    let audio = streams
        .iter()
        .find(|stream| stream["codec_type"] == "audio")
        .unwrap();
    assert_eq!(video["codec_name"], "h264");
    assert_eq!(video["width"], 64);
    assert_eq!(video["height"], 48);
    assert_eq!(video["pix_fmt"], "yuv420p");
    assert_eq!(video["r_frame_rate"], "30/1");
    assert_eq!(video["avg_frame_rate"], "30/1");
    assert!((0.0..=0.1).contains(&numeric_string(video, "start_time")));
    assert!(numeric_string(video, "duration") >= minimum_seconds);
    assert!(count(video, "nb_read_frames") >= minimum_video_frames);
    assert!(count(video, "nb_read_packets") >= minimum_video_frames);

    assert_eq!(audio["codec_name"], "aac");
    assert_eq!(audio["sample_rate"], "48000");
    assert_eq!(audio["channels"], 2);
    assert_eq!(audio["channel_layout"], "stereo");
    assert!(numeric_string(audio, "start_time").abs() < 0.05);
    assert!(numeric_string(audio, "duration") >= minimum_seconds);
    assert!(count(audio, "nb_read_frames") > 0);
    assert!(count(audio, "nb_read_packets") > 0);
    assert!(numeric_string(&probe["format"], "duration") >= minimum_seconds);

    let decode = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .unwrap();
    assert!(
        decode.status.success(),
        "decode failed: {}",
        String::from_utf8_lossy(&decode.stderr)
    );

    let decoded_audio = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "f32le", "-"])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        decoded_audio.status.success(),
        "audio decode failed: {}",
        String::from_utf8_lossy(&decoded_audio.stderr)
    );
    assert_eq!(decoded_audio.stdout.len() % (2 * size_of::<f32>()), 0);
    let decoded_sample_frames = decoded_audio.stdout.len() / (2 * size_of::<f32>());
    assert!(
        decoded_sample_frames >= usize::try_from(minimum_video_frames).unwrap() * 1_500,
        "only {decoded_sample_frames} decoded AAC sample frames"
    );
}

fn assert_wav_sidecar(path: &std::path::Path, expected_seconds: f64) {
    let probe = probe(path);
    let streams = probe["streams"].as_array().unwrap();
    let audio = streams
        .iter()
        .find(|stream| stream["codec_type"] == "audio")
        .expect("WAV sidecar has no audio stream");
    assert_eq!(audio["codec_name"], "pcm_s16le");
    assert_eq!(audio["sample_rate"], "48000");
    assert_eq!(audio["channels"], 2);
    let duration = numeric_string(audio, "duration");
    assert!(
        (duration - expected_seconds).abs() <= 0.05,
        "WAV stream duration {duration} does not match {expected_seconds}"
    );
    let container_duration = numeric_string(&probe["format"], "duration");
    assert!(
        (container_duration - expected_seconds).abs() <= 0.05,
        "WAV container duration {container_duration} does not match {expected_seconds}"
    );

    let decode = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .unwrap();
    assert!(
        decode.status.success(),
        "WAV decode failed: {}",
        String::from_utf8_lossy(&decode.stderr)
    );
}

#[test]
fn safe_stop_produces_playable_fragmented_h264_aac_mp4() {
    if !tools_available() {
        return;
    }
    let directory = tempdir().unwrap();
    let path = directory.path().join("recording.mp4");
    let output = std::fs::File::create(&path).unwrap();
    let format = format();
    let mut recorder = Recorder::start(output, RecordConfig::new(format.clone())).unwrap();
    enqueue_all(&mut recorder, &format);
    let report = recorder.stop();
    assert_eq!(report.outcome, StopOutcome::Clean, "{report:?}");
    assert_eq!(report.output, OutputFinalization::Synced);
    assert_eq!(report.cleanup, CleanupStatus::Complete);
    assert_eq!(report.telemetry.completed_pairs, FRAME_COUNT);
    assert_eq!(recorder.stop(), report);
    assert_eq!(recorder.telemetry(), report.telemetry);
    let probe = probe(&path);
    let video = probe["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .unwrap();
    assert_eq!(count(video, "nb_read_frames"), FRAME_COUNT);
    assert_eq!(count(video, "nb_read_packets"), FRAME_COUNT);
    assert_probe_and_decode(&path, FRAME_COUNT);
}

#[test]
fn safe_stop_writes_a_matching_wav_sidecar_alongside_the_mp4() {
    if !tools_available() {
        return;
    }
    let directory = tempdir().unwrap();
    let path = directory.path().join("recording.mp4");
    let wav_path = directory.path().join("recording.wav");
    let output = std::fs::File::create(&path).unwrap();
    let format = format();
    let mut config = RecordConfig::new(format.clone());
    config.wav = Some(wav_path.clone());
    let mut recorder = Recorder::start(output, config).unwrap();
    enqueue_all(&mut recorder, &format);
    let report = recorder.stop();
    assert_eq!(report.outcome, StopOutcome::Clean, "{report:?}");
    assert_eq!(report.output, OutputFinalization::Synced, "{report:?}");
    assert_eq!(report.cleanup, CleanupStatus::Complete, "{report:?}");
    assert_eq!(report.telemetry.completed_pairs, FRAME_COUNT);
    assert_eq!(recorder.stop(), report);
    assert_eq!(recorder.telemetry(), report.telemetry);

    let wav_length = std::fs::metadata(&wav_path).unwrap().len();
    assert_eq!(report.wav_bytes, wav_length, "{report:?}");
    assert_eq!(report.telemetry.wav_bytes, wav_length);
    assert!(
        report.wav_bytes > 44,
        "WAV sidecar has no payload: {report:?}"
    );

    assert_probe_and_decode(&path, FRAME_COUNT);
    let probe = probe(&path);
    let video = probe["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .unwrap();
    assert_eq!(count(video, "nb_read_frames"), FRAME_COUNT);
    assert_eq!(count(video, "nb_read_packets"), FRAME_COUNT);
    let span_seconds = f64::from(u32::try_from(FRAME_COUNT).unwrap()) / 30.0;
    assert_wav_sidecar(&wav_path, span_seconds);
}

#[test]
fn second_start_refuses_an_existing_wav_sidecar_path() {
    if !tools_available() {
        return;
    }
    let directory = tempdir().unwrap();
    let format = format();
    let wav_path = directory.path().join("existing.wav");
    let first_output = std::fs::File::create(directory.path().join("first.mp4")).unwrap();
    let mut first_config = RecordConfig::new(format.clone());
    first_config.wav = Some(wav_path.clone());
    let mut recorder = Recorder::start(first_output, first_config).unwrap();

    let mut second_config = RecordConfig::new(format);
    second_config.wav = Some(wav_path.clone());
    let second_output = std::fs::File::create(directory.path().join("second.mp4")).unwrap();
    let Err(error) = Recorder::start(second_output, second_config) else {
        panic!("start accepted the existing WAV sidecar");
    };
    assert_eq!(
        error.kind,
        StartErrorKind::WavOutput(std::io::ErrorKind::AlreadyExists)
    );
    assert_eq!(error.cleanup, CleanupStatus::Complete);

    let report = recorder.stop();
    assert_eq!(report.outcome, StopOutcome::Clean, "{report:?}");
    assert_eq!(report.cleanup, CleanupStatus::Complete, "{report:?}");
    let wav_length = std::fs::metadata(&wav_path).unwrap().len();
    assert_eq!(report.wav_bytes, wav_length, "{report:?}");
}

#[test]
fn forced_child_kill_leaves_a_playable_fragmented_prefix() {
    if !tools_available() {
        return;
    }
    let directory = tempdir().unwrap();
    let path = directory.path().join("killed-prefix.mp4");
    let output = std::fs::File::create(&path).unwrap();
    let format = format();
    let mut recorder = Recorder::start(output, RecordConfig::new(format.clone())).unwrap();
    enqueue_all(&mut recorder, &format);
    let deadline = Instant::now() + Duration::from_secs(5);
    while recorder.telemetry().completed_pairs < FRAME_COUNT && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(recorder.telemetry().completed_pairs, FRAME_COUNT);
    // Let the child mux what it has already read. A fixed sleep is too tight
    // on a loaded host and can kill the child before it has written a full
    // frame; wait for the output to stop growing, bounded.
    let settle_deadline = Instant::now() + Duration::from_secs(5);
    let mut last_bytes = recorder.telemetry().output_bytes;
    let mut stable_polls = 0;
    while Instant::now() < settle_deadline && stable_polls < 4 {
        thread::sleep(Duration::from_millis(25));
        let bytes = recorder.telemetry().output_bytes;
        if bytes == last_bytes {
            stable_polls += 1;
        } else {
            stable_polls = 0;
            last_bytes = bytes;
        }
    }
    let report = recorder.cancel();
    assert_eq!(report.outcome, StopOutcome::Killed, "{report:?}");
    assert_eq!(report.output, OutputFinalization::Synced, "{report:?}");
    assert_eq!(report.cleanup, CleanupStatus::Complete, "{report:?}");
    assert_eq!(recorder.cancel(), report);
    assert_eq!(recorder.telemetry(), report.telemetry);
    assert_probe_and_decode(&path, 30);
}

#[test]
fn idle_safe_stop_has_no_second_input_timeout() {
    if !tools_available() {
        return;
    }
    let directory = tempdir().unwrap();
    let path = directory.path().join("idle.mp4");
    let output = std::fs::File::create(path).unwrap();
    let mut recorder = Recorder::start(output, RecordConfig::new(format())).unwrap();
    let report = recorder.stop();
    assert_eq!(report.outcome, StopOutcome::Clean, "{report:?}");
    assert_eq!(report.output, OutputFinalization::Synced, "{report:?}");
    assert_eq!(report.cleanup, CleanupStatus::Complete, "{report:?}");
    assert_eq!(report.telemetry.accepted_pairs, 0);
    assert_eq!(report.telemetry.completed_pairs, 0);
    assert_eq!(report.telemetry.failure, None);
}
