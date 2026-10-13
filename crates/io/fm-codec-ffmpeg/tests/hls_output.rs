//! Local HLS output integration coverage.
//!
//! # Why this is a filesystem test
//!
//! An HLS destination is the one output this crate can observe whole: the
//! child writes a playlist plus MPEG-TS segments into one absolute directory,
//! so "the stream is real" means the playlist parses, the segments exist, and
//! ffprobe decodes the first segment as H.264 video with AAC audio. Byte
//! counting would prove none of that.
//!
//! The same gating as the RTMP sink tests applies: cases skip when `ffmpeg` or
//! `ffprobe` is unavailable, unless `FM_REQUIRE_FFMPEG=1` makes that a failure.
//!
//! # Media shape
//!
//! The producer is the wall-clock-paced 320x240 30 fps stereo producer the RTMP
//! tests use. The keyframe interval equals the HLS segment duration, so a few
//! seconds of media must yield at least two complete segments. `Streamer::start`
//! creates the leaf directory itself, which is the creation path this test
//! exercises: the directory is deliberately missing before startup.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::stream::{
    CleanupStatus, PairedFrame, RecordFormat, StartErrorKind, StopOutcome, StreamConfig,
    StreamDestination, Streamer,
};
use fm_frame::{
    AudioBlock, ChannelLayout, ClockDomainId, MediaTimestamp, MediaTiming, NormalizedDuration,
    NormalizedTimestamp, OriginalTimestamp, SampleRate, SequenceNumber, TimeBase,
};
use fm_types::FrameRate;
use tempfile::tempdir;

const FPS: u64 = 30;
const FRAME_PERIOD: Duration = Duration::from_nanos(1_000_000_000 / FPS);
const SAMPLES_PER_FRAME: u64 = 1_600;
const FIRST_SEQUENCE: u64 = 0;
/// 4.5 seconds at 30 fps: enough for two complete 2 s segments plus a final
/// partial one.
const MEDIA_FRAMES: u64 = 135;
const KEYFRAME_INTERVAL_SECONDS: u32 = 2;

static SERIAL: Mutex<()> = Mutex::new(());

/// `FFmpeg` children are CPU heavy on a 4-core box; run one scenario at a time.
fn serialize() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
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
    available
}

fn format(width: u32, height: u32) -> RecordFormat {
    RecordFormat::new(
        width,
        height,
        FrameRate::new(u32::try_from(FPS).unwrap(), 1).unwrap(),
        SampleRate::new(48_000).unwrap(),
        ChannelLayout::stereo(),
        SequenceNumber::new(FIRST_SEQUENCE),
    )
    .unwrap()
}

fn frame(format: &RecordFormat, offset: u64) -> PairedFrame {
    let sequence = SequenceNumber::new(format.first_sequence().get() + offset);
    let start_sample = sequence.get() * SAMPLES_PER_FRAME;
    let start_nanos = sequence.get() * 1_000_000_000 / FPS;
    let end_nanos = (sequence.get() + 1) * 1_000_000_000 / FPS;
    let timing = MediaTiming::new(
        OriginalTimestamp::new(
            MediaTimestamp::new(i64::try_from(start_sample).unwrap()),
            TimeBase::new(1, 48_000).unwrap(),
        ),
        NormalizedTimestamp::from_nanos(i64::try_from(start_nanos).unwrap()),
        NormalizedDuration::from_nanos(end_nanos - start_nanos).unwrap(),
        ClockDomainId::new(std::num::NonZeroU128::new(7).unwrap()),
        sequence,
    )
    .unwrap();
    // A per-frame sawtooth: real, finite, non-silent audio without lossy casts.
    let tone = (0..SAMPLES_PER_FRAME)
        .map(|index| {
            let step = (index + offset * 37) % 400;
            f32::from(u16::try_from(step).unwrap()) / 200.0 - 1.0
        })
        .collect::<Vec<_>>();
    let audio = AudioBlock::new(
        timing,
        format.sample_rate(),
        format.channel_layout().clone(),
        vec![tone.clone(), tone],
    )
    .unwrap();
    // Moving detail in every frame: a static picture would let x264 emit
    // near-empty frames and hide any real throughput problem.
    let width = usize::try_from(format.dimensions().width()).unwrap();
    let mut rgba = vec![0_u8; format.rgba_bytes_per_frame()];
    for (row, line) in rgba.chunks_exact_mut(width * 4).enumerate() {
        let base = u8::try_from((row + usize::try_from(offset).unwrap() * 11) % 255).unwrap();
        for (column, pixel) in line.chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(&[
                base,
                u8::try_from(column % 251).unwrap(),
                u8::try_from((offset * 3) % 255).unwrap(),
                255,
            ]);
        }
    }
    PairedFrame::new(format, sequence, rgba, audio).unwrap()
}

/// Runs the wall-clock-paced producer the RTMP tests use: the sequence is
/// derived from elapsed time, so a sink that fell behind would skip ahead
/// rather than stretch the timeline. Returns the wall-clock span covered.
fn produce(streamer: &mut Streamer, format: &RecordFormat, frames: u64) -> Duration {
    let started = Instant::now();
    let mut next = 0_u64;
    while next < frames {
        let target = started + FRAME_PERIOD * u32::try_from(next).unwrap();
        let now = Instant::now();
        if target > now {
            thread::sleep(target - now);
        }
        let elapsed = u64::try_from(started.elapsed().as_nanos() / FRAME_PERIOD.as_nanos())
            .unwrap_or(u64::MAX);
        let offset = elapsed.max(next);
        next = offset + 1;
        // A live render thread never retries: a refused frame is simply gone,
        // and the sink is responsible for keeping the timeline whole anyway.
        let _ = streamer.enqueue(frame(format, offset));
    }
    FRAME_PERIOD * u32::try_from(next).unwrap()
}

/// True while any process is still running our publisher command line. The
/// `rawvideo` term distinguishes the sink's own child from anything else that
/// happens to mention the directory. A reparented orphan is still found.
fn publisher_alive(marker: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        std::fs::read(entry.path().join("cmdline")).is_ok_and(|cmdline| {
            let text = String::from_utf8_lossy(&cmdline);
            text.contains(marker) && text.contains("rawvideo")
        })
    })
}

/// Every codec `ffprobe` reports for the media in one file.
fn probe_codecs(path: &Path) -> Vec<(String, String)> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let probe: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    probe["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|stream| {
            (
                stream["codec_type"].as_str().unwrap().to_owned(),
                stream["codec_name"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn local_hls_output_writes_a_playable_playlist_and_segments() {
    let _guard = serialize();
    if !tools_available() {
        return;
    }
    let root = tempdir().unwrap();
    // The leaf is deliberately missing before startup: `Streamer::start` must
    // create exactly this one directory.
    let directory = root.path().join("hls");
    let directory_text = directory.to_str().unwrap().to_owned();
    let url = format!("hls://{directory_text}");
    let destination = StreamDestination::parse(&url).unwrap();
    assert_eq!(destination.local_directory(), Some(directory_text.as_str()));
    // HLS has no secret: redaction must leave the path untouched.
    assert_eq!(destination.redacted(), url);

    let format = format(320, 240);
    let mut config = StreamConfig::new(format.clone(), destination);
    config.encoder.keyframe_interval_seconds = KEYFRAME_INTERVAL_SECONDS;
    let mut streamer = Streamer::start(config).unwrap();
    assert!(
        directory.is_dir(),
        "the streamer must create the HLS leaf directory"
    );
    assert_eq!(streamer.destination(), url);

    let wall = produce(&mut streamer, &format, MEDIA_FRAMES);
    assert!(
        wall >= Duration::from_secs(4),
        "the producer only covered {wall:?}"
    );
    let report = streamer.stop();
    assert_eq!(report.outcome, StopOutcome::Clean, "{report:?}");
    assert_eq!(report.cleanup, CleanupStatus::Complete, "{report:?}");
    assert_eq!(report.telemetry.failure, None, "{report:?}");
    assert!(report.telemetry.connected, "{report:?}");
    assert_eq!(streamer.stop(), report, "stop must be idempotent");

    let playlist = std::fs::read_to_string(directory.join("index.m3u8")).unwrap();
    assert!(playlist.starts_with("#EXTM3U"), "{playlist}");
    assert!(playlist.contains("#EXT-X-TARGETDURATION"), "{playlist}");
    let references = playlist
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with('#')
                && Path::new(line)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("ts"))
        })
        .collect::<Vec<_>>();
    assert!(
        references.len() >= 2,
        "the playlist references {} segments: {playlist}",
        references.len()
    );
    for reference in &references {
        assert!(
            directory.join(reference).is_file(),
            "playlist references a missing segment {reference}: {playlist}"
        );
    }

    let mut segments = std::fs::read_dir(&directory)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("segment-")
                        && Path::new(name)
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("ts"))
                })
        })
        .collect::<Vec<_>>();
    segments.sort();
    assert!(
        segments.len() >= 2,
        "expected at least two segments, found {segments:?}"
    );
    for segment in &segments {
        let size = std::fs::metadata(segment).unwrap().len();
        assert!(size > 0, "{segment:?} is empty");
    }

    let codecs = probe_codecs(&segments[0]);
    assert!(
        codecs
            .iter()
            .any(|(kind, codec)| kind == "video" && codec == "h264"),
        "first segment has no H.264 video: {codecs:?}"
    );
    assert!(
        codecs
            .iter()
            .any(|(kind, codec)| kind == "audio" && codec == "aac"),
        "first segment has no AAC audio: {codecs:?}"
    );

    assert!(
        !publisher_alive(&directory_text),
        "an ffmpeg publisher survived stop"
    );
}

#[test]
fn start_refuses_an_hls_destination_that_is_a_regular_file() {
    let _guard = serialize();
    if !tools_available() {
        return;
    }
    let root = tempdir().unwrap();
    let path = root.path().join("not-a-directory");
    std::fs::write(&path, b"occupied").unwrap();
    let path_text = path.to_str().unwrap().to_owned();
    let destination = StreamDestination::parse(&format!("hls://{path_text}")).unwrap();
    let config = StreamConfig::new(format(320, 240), destination);

    let Err(error) = Streamer::start(config) else {
        panic!("a regular file must not be accepted as an HLS destination");
    };
    assert_eq!(
        error.kind,
        StartErrorKind::HlsDirectoryUnusable,
        "{error:?}"
    );
    assert_eq!(error.cleanup, CleanupStatus::Complete, "{error:?}");
    // The check runs before any child exists, so no publisher may be alive for
    // this directory and the file is left untouched.
    assert!(
        !publisher_alive(&path_text),
        "a child was spawned for a refused HLS destination"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"occupied");
}
