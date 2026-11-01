use std::num::NonZeroU128;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use std::sync::{Arc, Mutex};

use fm_codec_ffmpeg::record::{
    CleanupStatus, EnqueueRejection, OutputFinalization, PairedFrame, RecordConfig, RecordFormat,
    RecordSink, Recorder, ResilientWavOutput, StartErrorKind, StopOutcome,
};
use fm_frame::{
    AudioBlock, ChannelLayout, ClockDomainId, MediaTimestamp, MediaTiming, NormalizedDuration,
    NormalizedTimestamp, OriginalTimestamp, SampleRate, SequenceNumber, TimeBase,
};
use fm_record::{
    FileStorage, RecordingSession, ResilientWavConfig, SegmentFormat, SegmentState, SessionConfig,
    SessionState, UnknownSpace, read_wav_info,
};
use fm_types::{FrameRate, Timecode};
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
    for pixel in rgba.as_chunks_mut::<4>().0 {
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

/// Frame count of a fragmented file that is still being written, or `None`
/// while it is not yet readable or has no video frame count yet.
fn live_video_frame_count(path: &std::path::Path) -> Option<u64> {
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_frames",
            "-count_packets",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !probe.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&probe.stdout).ok()?;
    let video = value["streams"]
        .as_array()?
        .iter()
        .find(|stream| stream["codec_type"] == "video")?;
    video["nb_read_frames"].as_str()?.parse().ok()
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
fn safe_stop_records_with_configured_video_and_audio_bitrates() {
    if !tools_available() {
        return;
    }
    let directory = tempdir().unwrap();
    let path = directory.path().join("bitrates.mp4");
    let output = std::fs::File::create(&path).unwrap();
    let format = format();
    let mut config = RecordConfig::new(format.clone());
    config.video_bitrate_kbps = Some(2_000);
    config.audio_bitrate_kbps = Some(96);
    let mut recorder = Recorder::start(output, config).unwrap();
    enqueue_all(&mut recorder, &format);
    let report = recorder.stop();
    assert_eq!(report.outcome, StopOutcome::Clean, "{report:?}");
    assert_eq!(report.output, OutputFinalization::Synced, "{report:?}");
    assert_eq!(report.cleanup, CleanupStatus::Complete, "{report:?}");
    assert_eq!(report.telemetry.completed_pairs, FRAME_COUNT);
    assert_eq!(recorder.stop(), report);
    assert_eq!(recorder.telemetry(), report.telemetry);
    // Bitrate targeting is deliberately loose at this size, so only the
    // stream identity and decodability are asserted.
    assert_probe_and_decode(&path, FRAME_COUNT);
}

#[test]
fn safe_stop_embeds_the_configured_start_timecode() {
    if !tools_available() {
        return;
    }
    let directory = tempdir().unwrap();
    let path = directory.path().join("timecode.mp4");
    let output = std::fs::File::create(&path).unwrap();
    let format = format();
    let mut config = RecordConfig::new(format.clone());
    config.timecode = Some(Timecode::new(1, 2, 3, 4, 30).unwrap());
    let mut recorder = Recorder::start(output, config).unwrap();
    enqueue_all(&mut recorder, &format);
    let report = recorder.stop();
    assert_eq!(report.outcome, StopOutcome::Clean, "{report:?}");
    assert_eq!(report.output, OutputFinalization::Synced, "{report:?}");
    assert_eq!(report.cleanup, CleanupStatus::Complete, "{report:?}");
    assert_eq!(report.telemetry.completed_pairs, FRAME_COUNT);
    assert_eq!(recorder.stop(), report);
    assert_eq!(recorder.telemetry(), report.telemetry);

    // The timecode adds only an MP4 `tmcd` data stream; the H.264 and AAC
    // streams still probe and decode as before.
    assert_probe_and_decode(&path, FRAME_COUNT);
    let probe = probe(&path);
    let streams = probe["streams"].as_array().unwrap();
    let video = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .unwrap();
    assert_eq!(count(video, "nb_read_frames"), FRAME_COUNT);
    assert_eq!(count(video, "nb_read_packets"), FRAME_COUNT);

    // `FFmpeg` 6.1 writes the start timecode as an MP4 `tmcd` timecode track
    // and mirrors it in the video stream's `tags.timecode`; either stable
    // representation is accepted, but the rendered value must be exact.
    let tmcd = streams
        .iter()
        .any(|stream| stream["codec_type"] == "data" && stream["codec_tag_string"] == "tmcd");
    let timecodes = streams
        .iter()
        .filter_map(|stream| stream["tags"]["timecode"].as_str())
        .chain(probe["format"]["tags"]["timecode"].as_str())
        .collect::<Vec<_>>();
    assert!(
        tmcd || !timecodes.is_empty(),
        "ffprobe reports neither a tmcd data stream nor a timecode tag: {probe}"
    );
    assert!(
        timecodes.contains(&"01:02:03:04"),
        "ffprobe does not render the configured 01:02:03:04 timecode: {probe}"
    );
}

/// Records everything a WAV sink is given, and how often it was flushed and
/// finished.
#[derive(Clone, Default)]
struct CapturedWav(Arc<Mutex<(Vec<u8>, usize, usize)>>);

impl RecordSink for CapturedWav {
    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.0.lock().unwrap().0.extend_from_slice(bytes);
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().1 += 1;
        Ok(())
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().2 += 1;
        Ok(())
    }
}

#[test]
fn wav_sink_receives_the_child_wav_stream_byte_exactly() {
    if !tools_available() {
        return;
    }
    let directory = tempdir().unwrap();
    let format = format();

    // Reference: the same frames with the WAV written to a sidecar path.
    let reference_path = directory.path().join("reference.wav");
    let mut config = RecordConfig::new(format.clone());
    config.wav = Some(reference_path.clone());
    let output = std::fs::File::create(directory.path().join("reference.mp4")).unwrap();
    let mut recorder = Recorder::start(output, config).unwrap();
    enqueue_all(&mut recorder, &format);
    let report = recorder.stop();
    assert_eq!(report.outcome, StopOutcome::Clean, "{report:?}");
    let reference = std::fs::read(&reference_path).unwrap();

    // A WAV path and a WAV sink together are refused before any child.
    let mut both = RecordConfig::new(format.clone());
    both.wav = Some(directory.path().join("both.wav"));
    let Err(error) = Recorder::start_with_sinks(
        Box::new(std::fs::File::create(directory.path().join("both.mp4")).unwrap()),
        Box::new(CapturedWav::default()),
        both,
    ) else {
        panic!("start accepted a WAV path and a WAV sink");
    };
    assert_eq!(
        error.kind,
        StartErrorKind::WavOutput(std::io::ErrorKind::InvalidInput)
    );
    assert!(!directory.path().join("both.wav").exists());

    let mp4_path = directory.path().join("sink.mp4");
    let captured = CapturedWav::default();
    let mut recorder = Recorder::start_with_sinks(
        Box::new(std::fs::File::create(&mp4_path).unwrap()),
        Box::new(captured.clone()),
        RecordConfig::new(format.clone()),
    )
    .unwrap();
    enqueue_all(&mut recorder, &format);
    let report = recorder.stop();
    assert_eq!(report.outcome, StopOutcome::Clean, "{report:?}");
    assert_eq!(report.output, OutputFinalization::Synced, "{report:?}");
    assert_eq!(report.cleanup, CleanupStatus::Complete, "{report:?}");
    assert_probe_and_decode(&mp4_path, FRAME_COUNT);

    let (streamed, flushes, finishes) = captured.0.lock().unwrap().clone();
    assert_eq!((flushes, finishes), (1, 1), "the sink is finished once");
    assert_eq!(report.wav_bytes, streamed.len() as u64, "{report:?}");
    assert_eq!(report.telemetry.wav_bytes, streamed.len() as u64);

    // The stream is the sidecar byte for byte, except that a non-seekable
    // destination keeps FFmpeg's placeholder RIFF and data sizes.
    let info = read_wav_info(&mut std::io::Cursor::new(streamed.as_slice())).unwrap();
    let reference_info = read_wav_info(&mut std::io::Cursor::new(reference.as_slice())).unwrap();
    assert_eq!(info.data_offset, reference_info.data_offset);
    assert_eq!(info.spec, reference_info.spec);
    let data_offset = usize::try_from(info.data_offset).unwrap();
    assert_eq!(streamed.len(), reference.len());
    assert_eq!(streamed[8..data_offset - 4], reference[8..data_offset - 4]);
    assert!(
        streamed[data_offset..] == reference[data_offset..],
        "sample data differs from the sidecar"
    );
    assert_eq!(
        reference_info.data_bytes,
        FRAME_COUNT * 1_600 * 2 * 2,
        "every accepted sample is in the WAV"
    );
}

#[test]
fn resilient_wav_output_writes_a_playable_session_segment() {
    if !tools_available() {
        return;
    }
    let directory = tempdir().unwrap();
    let format = format();
    let session_config = SessionConfig::new("recording", directory.path())
        .unwrap()
        .with_segment_format(SegmentFormat::Wav)
        .with_bare_first_segment();
    let session = RecordingSession::start(session_config, FileStorage::new(UnknownSpace)).unwrap();
    let wav = ResilientWavOutput::new(session, ResilientWavConfig::default());
    let status = wav.status();
    let mp4_path = directory.path().join("recording.mp4");
    let mut recorder = Recorder::start_with_sinks(
        Box::new(std::fs::File::create(&mp4_path).unwrap()),
        Box::new(wav),
        RecordConfig::new(format.clone()),
    )
    .unwrap();
    enqueue_all(&mut recorder, &format);
    let report = recorder.stop();
    assert_eq!(report.outcome, StopOutcome::Clean, "{report:?}");
    assert_eq!(report.cleanup, CleanupStatus::Complete, "{report:?}");

    let state = status.get();
    assert!(state.finished);
    assert!(state.error.is_none(), "{state:?}");
    assert_eq!(state.snapshot.state, SessionState::Stopped);
    assert_eq!(state.segments.len(), 1);
    assert_eq!(state.segments[0].state, SegmentState::Closed);
    let wav_path = directory.path().join("recording.wav");
    let bytes = std::fs::read(&wav_path).unwrap();
    assert_eq!(state.segments[0].bytes, bytes.len() as u64);
    let info = read_wav_info(&mut std::io::Cursor::new(bytes.as_slice())).unwrap();
    assert_eq!(info.riff_size, bytes.len() as u64 - 8, "RIFF size patched");
    assert_eq!(
        info.data_bytes,
        FRAME_COUNT * 1_600 * 2 * 2,
        "data size patched"
    );
    assert!(directory.path().join("recording.wav.fmsession").exists());
    let span_seconds = f64::from(u32::try_from(FRAME_COUNT).unwrap()) / 30.0;
    assert_wav_sidecar(&wav_path, span_seconds);
    assert_probe_and_decode(&mp4_path, FRAME_COUNT);
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
    // Wait until the child has actually muxed the promised prefix before the
    // forced kill. A fixed sleep or an output-size plateau is too tight on a
    // loaded host: the muxer can stall between fragments while starved. The
    // fragmented file is readable while it is written, so poll its frame count
    // instead and keep the kill's contract exact: at least 30 muxed frames.
    let settle_deadline = Instant::now() + Duration::from_secs(30);
    while live_video_frame_count(&path).is_none_or(|frames| frames < 30)
        && Instant::now() < settle_deadline
    {
        thread::sleep(Duration::from_millis(25));
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

// ---------------------------------------------------------------------------
// Fault-tolerant output through `ResilientOutput`
// ---------------------------------------------------------------------------

mod resilient {
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use fm_codec_ffmpeg::record::{
        CleanupStatus, EnqueueRejection, OutputFinalization, RecordConfig, RecordFormat, Recorder,
        ResilientOutput, ResilientOutputStatus, StopOutcome,
    };
    use fm_record::{
        FailureKind, FailureOp, FileStorage, RecordingSession, ResilientWriterConfig, SegmentSink,
        SegmentState, SegmentStorage, SessionConfig, SessionState, SpaceProbe, UnknownSpace,
    };
    use tempfile::tempdir;

    use super::{FRAME_COUNT, count, format, frame, probe, tools_available};

    /// Enqueues frames `range` and waits until each was accepted.
    fn enqueue_range(recorder: &mut Recorder, format: &RecordFormat, range: std::ops::Range<u64>) {
        for offset in range {
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

    fn wait_for(
        status: &ResilientOutputStatus,
        what: &str,
        done: impl Fn(&ResilientOutputStatus) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done(status) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?}",
                status.get()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Video frames that decode from one segment, after a full decode pass.
    fn decoded_video_frames(path: &Path) -> u64 {
        let decode = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-map", "0:v:0", "-map", "0:a:0", "-f", "null", "-"])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            decode.status.success() && decode.stderr.is_empty(),
            "{} does not decode cleanly: {}",
            path.display(),
            String::from_utf8_lossy(&decode.stderr)
        );
        let probe = probe(path);
        let video = probe["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stream| stream["codec_type"] == "video")
            .unwrap_or_else(|| panic!("{} has no video stream", path.display()));
        count(video, "nb_read_frames")
    }

    fn segment_paths(config: &SessionConfig, status: &ResilientOutputStatus) -> Vec<PathBuf> {
        status
            .get()
            .segments
            .iter()
            .filter(|entry| {
                matches!(entry.state, SegmentState::Closed | SegmentState::Failed)
                    && entry.bytes > 0
            })
            .map(|entry| config.segment_path(entry.root, entry.index).unwrap())
            .collect()
    }

    /// Segment storage whose primary root runs out of space part-way through
    /// its `fail_on_write`-th write, exactly like a filling disk: half of the
    /// bytes land, then `ENOSPC`.
    struct FillingPrimary {
        primary: PathBuf,
        fail_on_write: u32,
        writes: Arc<AtomicU32>,
    }

    struct FillingSink {
        file: File,
        fails: Option<(u32, Arc<AtomicU32>)>,
    }

    impl SpaceProbe for FillingPrimary {
        fn available_bytes(&mut self, _directory: &Path) -> io::Result<u64> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
    }

    impl SegmentStorage for FillingPrimary {
        type Sink = FillingSink;

        fn create_segment(&mut self, path: &Path) -> io::Result<FillingSink> {
            let file = OpenOptions::new().write(true).create_new(true).open(path)?;
            let on_primary = path.parent() == Some(self.primary.as_path());
            Ok(FillingSink {
                file,
                fails: on_primary.then(|| (self.fail_on_write, Arc::clone(&self.writes))),
            })
        }
    }

    impl SegmentSink for FillingSink {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            if let Some((fail_on, writes)) = &self.fails
                && writes.fetch_add(1, Ordering::SeqCst) + 1 >= *fail_on
            {
                Write::write_all(&mut self.file, &bytes[..bytes.len() / 2])?;
                return Err(io::Error::from_raw_os_error(28));
            }
            Write::write_all(&mut self.file, bytes)
        }

        fn sync_data(&mut self) -> io::Result<()> {
            self.file.sync_data()
        }

        fn truncate(&mut self, length: u64) -> io::Result<()> {
            SegmentSink::truncate(&mut self.file, length)
        }
    }

    #[test]
    fn a_primary_that_fills_mid_fragment_fails_over_without_losing_a_frame() {
        if !tools_available() {
            return;
        }
        let directory = tempdir().unwrap();
        let primary = directory.path().join("primary");
        let fallback = directory.path().join("fallback");
        let config = SessionConfig::new("filling", &primary)
            .unwrap()
            .with_fallback(&fallback)
            .unwrap()
            .with_bare_first_segment();
        let writes = Arc::new(AtomicU32::new(0));
        let storage = FillingPrimary {
            primary: primary.clone(),
            // Write 1 is the init segment and write 2 the first fragment; the
            // disk fills while the second fragment is being written.
            fail_on_write: 3,
            writes: Arc::clone(&writes),
        };
        let session = RecordingSession::start(config.clone(), storage).unwrap();
        let output = ResilientOutput::new(session, ResilientWriterConfig::default());
        let status = output.status();
        let format = format();
        let mut recorder =
            Recorder::start_with_sink(Box::new(output), RecordConfig::new(format.clone())).unwrap();
        enqueue_range(&mut recorder, &format, 0..FRAME_COUNT);
        let report = recorder.stop();
        assert_eq!(report.outcome, StopOutcome::Clean, "{report:?}");
        assert_eq!(report.output, OutputFinalization::Synced, "{report:?}");
        assert_eq!(report.cleanup, CleanupStatus::Complete, "{report:?}");
        assert_eq!(report.telemetry.failure, None, "{report:?}");

        let state = status.get();
        assert!(state.finished);
        assert_eq!(state.error, None);
        assert_eq!(state.stats.failovers, 1, "{state:?}");
        assert_eq!(state.stats.fragments_rewritten, 1, "{state:?}");
        assert_eq!(state.snapshot.state, SessionState::Stopped);
        assert_eq!(state.snapshot.disabled_roots, [0]);
        assert_eq!(state.snapshot.active_root, 1);
        let full = state
            .snapshot
            .errors
            .iter()
            .find(|error| error.op == FailureOp::Write)
            .expect("the full disk is recorded");
        assert_eq!((full.root, full.kind), (0, FailureKind::StorageFull));

        // The primary keeps its init plus first fragment, truncated back to
        // the fragment boundary; everything after lives in the fallback.
        let segments = segment_paths(&config, &status);
        assert_eq!(
            segments,
            [
                primary.join("filling.mp4"),
                fallback.join("filling-0002.mp4")
            ],
            "{state:?}"
        );
        let frames: u64 = segments.iter().map(|path| decoded_video_frames(path)).sum();
        assert_eq!(frames, FRAME_COUNT, "every frame survives the failover");
    }

    #[test]
    #[cfg(unix)]
    fn a_primary_made_unwritable_mid_recording_continues_in_the_fallback() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        if !tools_available() {
            return;
        }
        let directory = tempdir().unwrap();
        let primary = directory.path().join("primary");
        let fallback = directory.path().join("fallback");
        let config = SessionConfig::new("chmod", &primary)
            .unwrap()
            .with_fallback(&fallback)
            .unwrap()
            .with_bare_first_segment();
        let session =
            RecordingSession::start(config.clone(), FileStorage::new(UnknownSpace)).unwrap();
        // One fragment per segment, so the next segment is created while
        // the primary is unwritable.
        let writer_config = ResilientWriterConfig {
            max_segment_fragments: std::num::NonZeroU64::new(1),
            ..ResilientWriterConfig::default()
        };
        let output = ResilientOutput::new(session, writer_config);
        let status = output.status();
        let format = format();
        let mut recorder =
            Recorder::start_with_sink(Box::new(output), RecordConfig::new(format.clone())).unwrap();
        // The encoder's lookahead holds about a second of frames, so the
        // first one-second fragment is muxed once frame ~75 is encoded.
        let frames_total: u64 = 150;
        enqueue_range(&mut recorder, &format, 0..75);
        wait_for(&status, "the first fragment", |status| {
            status.get().stats.fragments_written >= 1
        });
        fs::set_permissions(&primary, fs::Permissions::from_mode(0o555)).unwrap();
        if OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(primary.join("probe"))
            .is_ok()
        {
            // Running as root: permissions do not bind; nothing to prove.
            fs::set_permissions(&primary, fs::Permissions::from_mode(0o755)).unwrap();
            drop(recorder.cancel());
            eprintln!("skipping: directory permissions do not bind for this user");
            return;
        }
        enqueue_range(&mut recorder, &format, 75..frames_total);
        let report = recorder.stop();
        fs::set_permissions(&primary, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(report.outcome, StopOutcome::Clean, "{report:?}");
        assert_eq!(report.output, OutputFinalization::Synced, "{report:?}");
        assert_eq!(report.telemetry.failure, None, "{report:?}");

        let state = status.get();
        assert_eq!(state.error, None, "{state:?}");
        assert_eq!(state.snapshot.disabled_roots, [0], "{state:?}");
        assert_eq!(state.snapshot.active_root, 1, "{state:?}");
        assert!(state.snapshot.errors.iter().any(|error| error.root == 0
            && error.op == FailureOp::CreateSegment
            && error.kind == FailureKind::PermissionDenied));
        // Primary segments first, then only fallback segments.
        let segments = segment_paths(&config, &status);
        let on_primary = segments
            .iter()
            .take_while(|path| path.starts_with(&primary))
            .count();
        assert!(on_primary >= 1, "{segments:?}");
        assert!(
            segments.len() > on_primary
                && segments[on_primary..]
                    .iter()
                    .all(|path| path.starts_with(&fallback)),
            "{segments:?}"
        );
        let frames: u64 = segments.iter().map(|path| decoded_video_frames(path)).sum();
        assert_eq!(frames, frames_total, "every frame survives the failover");
        // The fallback manifest is current and closed.
        let manifest = fm_record::SessionManifest::parse(
            &fs::read_to_string(fallback.join("chmod.fmsession")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.state, SessionState::Stopped);
    }

    #[test]
    fn a_killed_muxer_leaves_a_stopped_session_of_whole_fragments() {
        if !tools_available() {
            return;
        }
        let directory = tempdir().unwrap();
        let primary = directory.path().join("primary");
        let config = SessionConfig::new("killed", &primary)
            .unwrap()
            .with_bare_first_segment();
        let session =
            RecordingSession::start(config.clone(), FileStorage::new(UnknownSpace)).unwrap();
        let output = ResilientOutput::new(session, ResilientWriterConfig::default());
        let status = output.status();
        let format = format();
        let mut config = RecordConfig::new(format.clone());
        // Cancel must wait for the output worker to close the current
        // fragment and sync it. A loaded CI host can take longer than the
        // production two-second kill budget, which would hand the file to the
        // asynchronous fallback cleanup and report `Unconfirmed`; this test
        // asserts the synchronous path instead.
        config.limits.kill_timeout = Duration::from_secs(15);
        let mut recorder = Recorder::start_with_sink(Box::new(output), config).unwrap();
        enqueue_range(&mut recorder, &format, 0..FRAME_COUNT);
        wait_for(&status, "a fragment", |status| {
            status.get().stats.fragments_written >= 1
        });
        let report = recorder.cancel();
        assert_eq!(report.outcome, StopOutcome::Killed, "{report:?}");
        // The session closes cleanly even though the stream was cut.
        assert_eq!(report.output, OutputFinalization::Synced, "{report:?}");
        let state = status.get();
        assert!(state.finished);
        assert_eq!(state.error, None, "{state:?}");
        assert_eq!(state.snapshot.state, SessionState::Stopped);
        let path = primary.join("killed.mp4");
        assert_eq!(
            fm_record::scan_fmp4_file(&path).unwrap().integrity,
            fm_record::Fmp4Integrity::Complete,
            "only whole fragments reach the segment"
        );
        assert!(decoded_video_frames(&path) >= 30);
    }
}
