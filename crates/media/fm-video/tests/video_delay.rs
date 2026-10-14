//! IN-022 reference slice: deterministic, bounded per-input video delay line.

#![allow(clippy::float_cmp)]

use std::num::NonZeroU128;
use std::sync::Arc;

use fm_audio::SampleDelay;
use fm_clock::{ClockTime, FrameCadence};
use fm_frame::{
    ClockDomainId, CpuVideoFrame, CpuVideoPayload, MediaFlags, MediaTimestamp, MediaTiming,
    NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, PixelFormat, SequenceNumber,
    TimeBase, VideoDimensions,
};
use fm_types::FrameRate;
use fm_video::{
    DelayChangePolicy, DelayDuration, DelayInputFrame, DelayLineState, DelayOutput,
    DelayedFrameKind, StartupPolicy, VideoDelayConfig, VideoDelayError, VideoDelayLine,
};

const NTSC_30: (u32, u32) = (30_000, 1_001);
const NTSC_60: (u32, u32) = (60_000, 1_001);
/// Arbitrary non-zero timeline origin so tests do not rely on starting at 0.
const ORIGIN: u64 = 7_000_000_000_123;

fn rate((numerator, denominator): (u32, u32)) -> FrameRate {
    FrameRate::new(numerator, denominator).unwrap()
}

struct Source {
    cadence: FrameCadence,
}

impl Source {
    fn new(fps: (u32, u32)) -> Self {
        Self {
            cadence: FrameCadence::new(rate(fps), ClockTime::from_nanos(ORIGIN)),
        }
    }

    fn pts(&self, index: u64) -> i64 {
        i64::try_from(self.cadence.time_of_frame(index).unwrap().as_nanos()).unwrap()
    }

    fn frame(&self, index: u64, bytes: usize) -> DelayInputFrame<u64> {
        let duration = self.cadence.duration_of_frame(index).unwrap().as_nanos();
        DelayInputFrame::new(self.pts(index), duration, bytes, index).unwrap()
    }
}

fn config(max_frames: usize, max_bytes: usize) -> VideoDelayConfig {
    VideoDelayConfig::new(
        DelayDuration::from_millis(5_000).unwrap(),
        max_bytes,
        max_frames,
    )
    .unwrap()
}

fn frames(count: u64, fps: (u32, u32)) -> DelayDuration {
    DelayDuration::from_frames(count, rate(fps)).unwrap()
}

/// Returns `(payload, kind, skipped)` or `None` for a start-up output.
fn out(line: &mut VideoDelayLine<u64>, now: i64) -> Option<(u64, DelayedFrameKind, u64)> {
    match line.output(now).unwrap() {
        DelayOutput::Frame(frame) => Some((*frame.payload, frame.kind, frame.skipped)),
        DelayOutput::Black | DelayOutput::Nothing => None,
    }
}

/// Pushes source frame `k` and outputs at the same timestamp.
fn tick(line: &mut VideoDelayLine<u64>, source: &Source, k: u64, bytes: usize) -> Option<u64> {
    let now = source.pts(k);
    line.push(now, source.frame(k, bytes)).unwrap();
    out(line, now).map(|(payload, _, _)| payload)
}

#[test]
fn delay_durations_convert_exactly_for_fractional_rates() {
    // 1001/30000 s = 33_366_666.67 ns; three frames are exactly 100.1 ms.
    assert_eq!(frames(1, NTSC_30).as_nanos(), 33_366_667);
    assert_eq!(frames(3, NTSC_30).as_nanos(), 100_100_000);
    assert_eq!(frames(1, NTSC_60).as_nanos(), 16_683_333);
    assert_eq!(
        DelayDuration::from_millis(1_000)
            .unwrap()
            .frames_at(rate(NTSC_30)),
        30
    );
    assert_eq!(frames(3, NTSC_30).frames_at(rate(NTSC_30)), 3);
    assert_eq!(frames(3, NTSC_30).audio_samples(48_000), 4_805);
    assert_eq!(
        DelayDuration::from_millis(100)
            .unwrap()
            .audio_samples(48_000),
        4_800
    );
    assert_eq!(
        DelayDuration::from_millis(u64::MAX),
        Err(VideoDelayError::ArithmeticOverflow)
    );
}

#[test]
fn frame_delay_at_29_97_selects_the_exact_frame_for_one_hour() {
    const DELAY: u64 = 5;
    // One hour at 30000/1001 is 107_892 frames.
    const FRAMES: u64 = 107_892;
    let source = Source::new(NTSC_30);
    let mut line = VideoDelayLine::new(config(64, usize::MAX), frames(DELAY, NTSC_30)).unwrap();

    for k in 0..FRAMES {
        let now = source.pts(k);
        line.push(now, source.frame(k, 0)).unwrap();
        let output = line.output(now).unwrap();
        if k < DELAY {
            assert_eq!(output, DelayOutput::Black, "frame {k}");
            assert_eq!(line.state(), DelayLineState::Filling);
        } else {
            let frame = output.frame().expect("running");
            assert_eq!(*frame.payload, k - DELAY, "frame {k}");
            assert_eq!(frame.kind, DelayedFrameKind::Fresh);
            assert_eq!(frame.skipped, 0);
            assert_eq!(frame.source_pts_nanos, source.pts(k - DELAY));
        }
    }

    let telemetry = line.telemetry();
    assert_eq!(telemetry.frames_pushed, FRAMES);
    assert_eq!(telemetry.frames_presented, FRAMES - DELAY);
    assert_eq!(telemetry.startup_outputs, DELAY);
    assert_eq!(telemetry.repeats, 0);
    assert_eq!(telemetry.skips, 0);
    assert_eq!(telemetry.capacity_drops, 0);
    assert_eq!(telemetry.discontinuities, 0);
    assert_eq!(telemetry.frames_held, usize::try_from(DELAY).unwrap() + 1);
    assert_eq!(telemetry.fill_per_mille, 1000);
}

#[test]
fn millisecond_delay_rounds_up_to_the_fractional_frame_grid() {
    // Sample-and-hold: the delay rounds up to the next frame start.
    // 1000 ms at 59.94 is 59.94 frames -> 60; 100 ms at 29.97 is 2.997 -> 3;
    // 110 ms at 29.97 is 3.297 -> 4.
    for (fps, millis, expected, count) in [
        (NTSC_60, 1_000, 60, 60 * 60 * 10),
        (NTSC_30, 100, 3, 30 * 60 * 10),
        (NTSC_30, 110, 4, 30 * 60 * 10),
    ] {
        let source = Source::new(fps);
        let delay = DelayDuration::from_millis(millis).unwrap();
        let mut line = VideoDelayLine::new(config(256, usize::MAX), delay).unwrap();
        for k in 0..count {
            let selected = tick(&mut line, &source, k, 0);
            assert_eq!(selected, k.checked_sub(expected), "{fps:?} frame {k}");
        }
    }
}

#[test]
fn faster_input_skips_and_slower_input_repeats_deterministically() {
    // 59.94 in, 29.97 out: every other source frame is skipped.
    let input = Source::new(NTSC_60);
    let output = Source::new(NTSC_30);
    let mut line = VideoDelayLine::new(config(64, usize::MAX), frames(4, NTSC_30)).unwrap();
    let mut pushed = 0;
    for j in 0..18_000 {
        let now = output.pts(j);
        while input.pts(pushed) <= now {
            line.push(input.pts(pushed), input.frame(pushed, 0))
                .unwrap();
            pushed += 1;
        }
        let selected = out(&mut line, now);
        if j >= 4 {
            let (payload, kind, skipped) = selected.unwrap();
            assert_eq!(payload, 2 * (j - 4), "output {j}");
            assert_eq!(kind, DelayedFrameKind::Fresh);
            assert_eq!(skipped, u64::from(j > 4));
        } else {
            assert_eq!(selected, None);
        }
    }
    assert_eq!(line.telemetry().skips, 18_000 - 5);

    // 29.97 in, 59.94 out: every source frame is shown twice.
    let input = Source::new(NTSC_30);
    let output = Source::new(NTSC_60);
    let mut line = VideoDelayLine::new(config(64, usize::MAX), frames(2, NTSC_30)).unwrap();
    let mut pushed = 0;
    for j in 0..36_000 {
        let now = output.pts(j);
        while input.pts(pushed) <= now {
            line.push(input.pts(pushed), input.frame(pushed, 0))
                .unwrap();
            pushed += 1;
        }
        let selected = out(&mut line, now);
        if j >= 4 {
            let (payload, kind, _) = selected.unwrap();
            assert_eq!(payload, (j - 4) / 2, "output {j}");
            let expected = if j % 2 == 0 {
                DelayedFrameKind::Fresh
            } else {
                DelayedFrameKind::Repeat
            };
            assert_eq!(kind, expected, "output {j}");
        } else {
            assert_eq!(selected, None);
        }
    }
    assert_eq!(line.telemetry().repeats, (36_000 - 4) / 2);
    assert_eq!(line.telemetry().skips, 0);
}

#[test]
fn startup_policy_outputs_nothing_or_black_until_filled() {
    let source = Source::new(NTSC_30);
    let cfg = config(32, usize::MAX).with_startup(StartupPolicy::Nothing);
    let mut line = VideoDelayLine::new(cfg, frames(3, NTSC_30)).unwrap();
    // Output before any input is also governed by the policy.
    assert_eq!(
        line.output(source.pts(0) - 1).unwrap(),
        DelayOutput::Nothing
    );
    for k in 0..3 {
        line.push(source.pts(k), source.frame(k, 0)).unwrap();
        assert_eq!(line.output(source.pts(k)).unwrap(), DelayOutput::Nothing);
        let telemetry = line.telemetry();
        assert_eq!(telemetry.state, DelayLineState::Filling);
        // Buffered k+1 frames of a 3-frame delay.
        assert_eq!(
            u64::from(telemetry.fill_per_mille),
            (k + 1) * 1000 / 3,
            "frame {k}"
        );
    }
    assert_eq!(tick(&mut line, &source, 3, 0), Some(0));
    assert_eq!(line.state(), DelayLineState::Running);
    assert_eq!(line.telemetry().startup_outputs, 4);

    // A zero delay passes frames straight through without a start-up output.
    let mut line = VideoDelayLine::new(config(2, usize::MAX), DelayDuration::ZERO).unwrap();
    for k in 0..100 {
        assert_eq!(tick(&mut line, &source, k, 0), Some(k));
    }
    assert_eq!(line.telemetry().frames_held, 1);
}

#[test]
fn byte_and_frame_bounds_are_never_exceeded() {
    const FRAME_BYTES: usize = 1_000;
    let source = Source::new(NTSC_30);
    // Room for 10 frames by bytes; the requested 30-frame delay cannot fit.
    let mut line = VideoDelayLine::new(config(64, 10_500), frames(30, NTSC_30)).unwrap();
    for k in 0..3_000 {
        let selected = tick(&mut line, &source, k, FRAME_BYTES);
        let telemetry = line.telemetry();
        assert!(telemetry.bytes_held <= 10_500, "frame {k}");
        assert!(telemetry.frames_held <= 10, "frame {k}");
        // Capacity pressure ends start-up at the first eviction (frame 10)
        // and the achievable delay is what fits: 9 frames.
        if k < 10 {
            assert_eq!(selected, None, "frame {k}");
        } else {
            assert_eq!(selected, Some(k - 9), "frame {k}");
        }
    }
    let telemetry = line.telemetry();
    assert_eq!(telemetry.bytes_held, 10 * FRAME_BYTES);
    assert_eq!(telemetry.early_presentations, 3_000 - 10);
    assert_eq!(telemetry.capacity_drops, 1);
    assert_eq!(telemetry.requested_delay, frames(30, NTSC_30));

    // Zero-byte handles are still bounded by the frame budget, even when no
    // output is drained at all.
    let mut line = VideoDelayLine::new(config(8, 1), frames(2, NTSC_30)).unwrap();
    for k in 0..1_000 {
        line.push(source.pts(k), source.frame(k, 0)).unwrap();
        assert!(line.telemetry().frames_held <= 8);
    }
    assert_eq!(line.telemetry().capacity_drops, 1_000 - 8);

    // An oversized frame is rejected without changing the line.
    let before = line.telemetry();
    assert_eq!(
        line.push(source.pts(1_000), source.frame(1_000, 2)),
        Err(VideoDelayError::FrameTooLarge {
            bytes: 2,
            maximum: 1
        })
    );
    assert_eq!(line.telemetry(), before);
}

#[test]
fn capacity_estimate_matches_the_largest_drop_free_delay() {
    const FRAME_BYTES: usize = 1_920 * 1_080 * 4;
    let source = Source::new(NTSC_30);
    let cfg = config(1_000, 20 * FRAME_BYTES + 17);
    let capacity = cfg.capacity_for(FRAME_BYTES, rate(NTSC_30)).unwrap();
    assert_eq!(capacity.frames_held, 20);
    assert_eq!(capacity.max_delay, frames(19, NTSC_30));
    // Zero-byte frames are limited by the frame budget and then max_delay.
    assert_eq!(
        cfg.capacity_for(0, rate(NTSC_30)).unwrap().max_delay,
        cfg.max_delay()
    );

    let mut line = VideoDelayLine::new(cfg, capacity.max_delay).unwrap();
    for k in 0..600 {
        assert_eq!(tick(&mut line, &source, k, FRAME_BYTES), k.checked_sub(19));
    }
    assert_eq!(line.telemetry().capacity_drops, 0);
    assert_eq!(line.telemetry().early_presentations, 0);

    let mut line = VideoDelayLine::new(cfg, frames(20, NTSC_30)).unwrap();
    for k in 0..600 {
        tick(&mut line, &source, k, FRAME_BYTES);
    }
    assert!(line.telemetry().capacity_drops > 0);
}

#[test]
fn delay_limits_are_validated_without_side_effects() {
    let cfg = VideoDelayConfig::new(DelayDuration::from_millis(500).unwrap(), 1, 4).unwrap();
    assert!(matches!(
        VideoDelayLine::<u64>::new(cfg, DelayDuration::from_millis(501).unwrap()),
        Err(VideoDelayError::DelayOutOfRange { .. })
    ));
    let mut line =
        VideoDelayLine::<u64>::new(cfg, DelayDuration::from_millis(200).unwrap()).unwrap();
    assert!(
        line.set_delay(DelayDuration::from_millis(600).unwrap())
            .is_err()
    );
    assert_eq!(
        line.requested_delay(),
        DelayDuration::from_millis(200).unwrap()
    );

    assert!(VideoDelayConfig::new(DelayDuration::from_millis(121_000).unwrap(), 1, 4).is_err());
    assert_eq!(
        VideoDelayConfig::new(DelayDuration::ZERO, 0, 4),
        Err(VideoDelayError::ZeroByteBudget)
    );
    assert!(VideoDelayConfig::new(DelayDuration::ZERO, 1, 1).is_err());
    assert!(VideoDelayConfig::new(DelayDuration::ZERO, 1, (1 << 16) + 1).is_err());
    assert_eq!(
        cfg.with_change_policy(DelayChangePolicy::Slew { per_mille: 0 }),
        Err(VideoDelayError::InvalidSlew { per_mille: 0 })
    );
    assert!(
        cfg.with_change_policy(DelayChangePolicy::Slew { per_mille: 1_001 })
            .is_err()
    );
    assert_eq!(
        cfg.with_max_skew(DelayDuration::ZERO),
        Err(VideoDelayError::ZeroSkew)
    );
    assert_eq!(
        DelayInputFrame::new(0, 0, 0, ()),
        Err(VideoDelayError::ZeroFrameDuration)
    );
}

#[test]
fn immediate_delay_changes_freeze_or_skip_by_exactly_the_change() {
    let source = Source::new(NTSC_30);
    let mut line = VideoDelayLine::new(config(128, usize::MAX), frames(10, NTSC_30)).unwrap();
    let mut k = 0;
    while k < 100 {
        assert_eq!(tick(&mut line, &source, k, 0), k.checked_sub(10));
        k += 1;
    }

    // +10 frames: the current frame (89) repeats for 10 outputs, then resumes.
    line.set_delay(frames(20, NTSC_30)).unwrap();
    for _ in 0..10 {
        let now = source.pts(k);
        line.push(now, source.frame(k, 0)).unwrap();
        assert_eq!(out(&mut line, now), Some((89, DelayedFrameKind::Repeat, 0)));
        k += 1;
    }
    for _ in 0..50 {
        assert_eq!(tick(&mut line, &source, k, 0), Some(k - 20));
        k += 1;
    }
    assert_eq!(line.telemetry().repeats, 10);
    assert_eq!(line.telemetry().frames_held, 21);

    // -16 frames: one output jumps forward, passing over exactly 16 frames.
    line.set_delay(frames(4, NTSC_30)).unwrap();
    let now = source.pts(k);
    line.push(now, source.frame(k, 0)).unwrap();
    assert_eq!(
        out(&mut line, now),
        Some((k - 4, DelayedFrameKind::Fresh, 16))
    );
    k += 1;
    for _ in 0..50 {
        assert_eq!(tick(&mut line, &source, k, 0), Some(k - 4));
        k += 1;
    }
    let telemetry = line.telemetry();
    assert_eq!(telemetry.skips, 16);
    assert_eq!(telemetry.frames_held, 5);
    assert_eq!(telemetry.effective_delay, frames(4, NTSC_30));
}

#[test]
fn slewed_delay_changes_converge_without_jumps() {
    let source = Source::new(NTSC_30);
    let cfg = config(128, usize::MAX)
        .with_change_policy(DelayChangePolicy::Slew { per_mille: 500 })
        .unwrap();
    let mut line = VideoDelayLine::new(cfg, frames(10, NTSC_30)).unwrap();
    let mut k = 0;
    let mut last = 0;
    while k < 60 {
        last = tick(&mut line, &source, k, 0).unwrap_or(0);
        k += 1;
    }
    assert_eq!(last, 49);

    // Increase by 6 frames at half speed: about 12 outputs (integer
    // nanosecond rounding adds one), each advancing by 0 or 1 frame.
    line.set_delay(frames(16, NTSC_30)).unwrap();
    let repeats_before = line.telemetry().repeats;
    let mut outputs = 0;
    while line.effective_delay() != frames(16, NTSC_30) {
        let selected = tick(&mut line, &source, k, 0).unwrap();
        assert!(selected - last <= 1, "advance {last} -> {selected}");
        last = selected;
        k += 1;
        outputs += 1;
        assert!(outputs <= 13, "slew did not converge");
    }
    assert!(outputs >= 12);
    for _ in 0..30 {
        assert_eq!(tick(&mut line, &source, k, 0), Some(k - 16));
        k += 1;
    }
    // Net source advance equals outputs minus the 6-frame increase.
    assert_eq!(line.telemetry().repeats - repeats_before, 6);
    last = k - 1 - 16;

    // Decrease by 12 frames at 1.5x: about 24 outputs, each advancing 1 or 2.
    line.set_delay(frames(4, NTSC_30)).unwrap();
    let skips_before = line.telemetry().skips;
    let mut outputs = 0;
    while line.effective_delay() != frames(4, NTSC_30) || outputs == 0 {
        let selected = tick(&mut line, &source, k, 0).unwrap();
        assert!((1..=2).contains(&(selected - last)), "{last} -> {selected}");
        last = selected;
        k += 1;
        outputs += 1;
        assert!(outputs <= 25, "slew did not converge");
    }
    assert!(outputs >= 24);
    for _ in 0..30 {
        assert_eq!(tick(&mut line, &source, k, 0), Some(k - 4));
        k += 1;
    }
    assert_eq!(line.telemetry().skips - skips_before, 12);
}

#[test]
fn timestamp_jumps_reset_and_reanchor_the_line() {
    const DELAY: u64 = 6;
    let source = Source::new(NTSC_30);
    let mut line = VideoDelayLine::new(config(64, usize::MAX), frames(DELAY, NTSC_30)).unwrap();
    for k in 0..100 {
        tick(&mut line, &source, k, 0);
    }

    // The source restarts at timestamp 0 (a backwards jump) while output
    // time keeps advancing; payloads 1000.. identify the new segment.
    let restarted = Source {
        cadence: FrameCadence::new(rate(NTSC_30), ClockTime::ZERO),
    };
    for j in 0..50 {
        let now = source.pts(100 + j);
        let duration = restarted.cadence.duration_of_frame(j).unwrap().as_nanos();
        let frame = DelayInputFrame::new(restarted.pts(j), duration, 0, 1_000 + j).unwrap();
        let report = line.push(now, frame).unwrap();
        assert_eq!(report.discontinuity, j == 0);
        let selected = out(&mut line, now);
        if j < DELAY {
            assert_eq!(selected, None, "segment frame {j}");
        } else {
            let (payload, kind, _) = selected.unwrap();
            assert_eq!(payload, 1_000 + j - DELAY, "segment frame {j}");
            assert_eq!(kind, DelayedFrameKind::Fresh);
        }
    }
    let telemetry = line.telemetry();
    assert_eq!(telemetry.discontinuities, 1);
    // Frames 94..=99 were buffered but never shown before the jump.
    assert_eq!(telemetry.discontinuity_drops, DELAY);
    assert_eq!(telemetry.startup_outputs, 2 * DELAY);

    // A forward jump of an hour beyond the skew tolerance also resets, and
    // the new segment is re-anchored to the output time.
    let now = source.pts(150);
    let far = source.pts(150) + 3_600_000_000_000;
    let report = line
        .push(
            now,
            DelayInputFrame::new(far, 33_366_667, 0, 9_000).unwrap(),
        )
        .unwrap();
    assert!(report.discontinuity);
    assert_eq!(line.state(), DelayLineState::Filling);
    for j in 1..=DELAY {
        let now = source.pts(150 + j);
        let pts = far + i64::try_from(j).unwrap() * 33_366_667;
        line.push(
            now,
            DelayInputFrame::new(pts, 33_366_667, 0, 9_000 + j).unwrap(),
        )
        .unwrap();
        let selected = out(&mut line, now);
        if j < DELAY {
            assert_eq!(selected, None);
        } else {
            assert_eq!(selected.map(|(payload, _, _)| payload), Some(9_000));
        }
    }

    // An explicitly flagged discontinuity resets even with ordinary spacing.
    let now = source.pts(160);
    let report = line
        .push(
            now,
            DelayInputFrame::new(far + 10 * 33_366_667, 33_366_667, 0, 0)
                .unwrap()
                .with_discontinuity(),
        )
        .unwrap();
    assert!(report.discontinuity);
    assert_eq!(line.telemetry().discontinuities, 3);
}

#[test]
fn gaps_hold_the_last_frame_without_resetting() {
    let source = Source::new(NTSC_30);
    let mut line = VideoDelayLine::new(config(64, usize::MAX), frames(3, NTSC_30)).unwrap();
    for k in 0..20 {
        tick(&mut line, &source, k, 0);
    }
    // Input stalls for five frame periods: output keeps advancing through
    // the buffered frames and then repeats the newest one.
    let mut shown = Vec::new();
    for k in 20..25 {
        shown.push(out(&mut line, source.pts(k)).unwrap());
    }
    assert_eq!(
        shown,
        vec![
            (17, DelayedFrameKind::Fresh, 0),
            (18, DelayedFrameKind::Fresh, 0),
            (19, DelayedFrameKind::Fresh, 0),
            (19, DelayedFrameKind::Repeat, 0),
            (19, DelayedFrameKind::Repeat, 0),
        ]
    );
    // Input resumes with correct timestamps: no discontinuity. Frames 20..25
    // never existed, so frame 19 holds until frame 25 becomes due.
    for k in 25..40 {
        let expected = if k < 28 { 19 } else { k - 3 };
        assert_eq!(tick(&mut line, &source, k, 0), Some(expected), "frame {k}");
    }
    let telemetry = line.telemetry();
    assert_eq!(telemetry.discontinuities, 0);
    assert_eq!(telemetry.skips, 0);
    assert_eq!(telemetry.repeats, 5);

    // Output time may not regress; the line is unchanged.
    let before = line.telemetry();
    assert!(matches!(
        line.output(source.pts(30)),
        Err(VideoDelayError::NowRegressed { .. })
    ));
    assert_eq!(line.telemetry(), before);
}

#[test]
fn explicit_reset_returns_to_filling() {
    let source = Source::new(NTSC_30);
    let mut line = VideoDelayLine::new(config(64, usize::MAX), frames(2, NTSC_30)).unwrap();
    for k in 0..10 {
        tick(&mut line, &source, k, 0);
    }
    line.reset();
    let telemetry = line.telemetry();
    assert_eq!(telemetry.resets, 1);
    assert_eq!(telemetry.frames_held, 0);
    assert_eq!(telemetry.state, DelayLineState::Filling);
    assert_eq!(telemetry.discontinuity_drops, 2);
    for k in 10..20 {
        let expected = (k >= 12).then(|| k - 2);
        assert_eq!(tick(&mut line, &source, k, 0), expected);
    }
}

#[test]
fn audio_delayed_by_the_exposed_delay_stays_in_sync_with_video() {
    const SAMPLE_RATE: u32 = 48_000;
    const FLASH: u64 = 90;
    let source = Source::new(NTSC_30);
    let mut line = VideoDelayLine::new(config(64, usize::MAX), frames(3, NTSC_30)).unwrap();

    // Audio delay derived from the video line's effective delay.
    let delay_samples = line.effective_delay().audio_samples(SAMPLE_RATE);
    assert_eq!(delay_samples, 4_805);
    let mut audio = SampleDelay::new(1, usize::try_from(delay_samples).unwrap()).unwrap();

    // The flash frame and an audio click happen at the same media time:
    // frame 90 starts exactly 3.003 s after the origin, i.e. sample 144_144.
    let flash_offset = u64::try_from(source.pts(FLASH)).unwrap() - ORIGIN;
    assert_eq!(flash_offset, 3_003_000_000);
    let click = usize::try_from(flash_offset * u64::from(SAMPLE_RATE) / 1_000_000_000).unwrap();
    let mut input = vec![0.0_f32; 200_000];
    input[click] = 1.0;
    let mut output = vec![0.0_f32; input.len()];
    for (source_block, output_block) in input.chunks(1_024).zip(output.chunks_mut(1_024)) {
        audio
            .process_into(&[source_block], &mut [output_block])
            .unwrap();
    }
    let audio_out = output.iter().position(|&sample| sample == 1.0).unwrap();
    let audio_nanos = u128::try_from(audio_out).unwrap() * 1_000_000_000 / u128::from(SAMPLE_RATE);

    let mut video_nanos = None;
    for k in 0..200 {
        let now = source.pts(k);
        line.push(now, source.frame(k, 0)).unwrap();
        if let Some((FLASH, DelayedFrameKind::Fresh, _)) = out(&mut line, now) {
            video_nanos = Some(u128::try_from(now).unwrap() - u128::from(ORIGIN));
        }
    }
    let video_nanos = video_nanos.expect("flash frame presented");
    // Within one audio sample period.
    assert!(audio_nanos.abs_diff(video_nanos) <= 1_000_000_000 / u128::from(SAMPLE_RATE));
}

#[test]
fn cpu_video_frames_are_delayed_by_shared_handle() {
    let dimensions = VideoDimensions::new(64, 36).unwrap();
    let domain = ClockDomainId::new(NonZeroU128::new(1).unwrap());
    let time_base = TimeBase::new(1, 1_000_000_000).unwrap();
    let cadence = FrameCadence::new(rate(NTSC_60), ClockTime::from_nanos(ORIGIN));
    let make = |index: u64, flags: MediaFlags| {
        let pts = i64::try_from(cadence.time_of_frame(index).unwrap().as_nanos()).unwrap();
        let duration = cadence.duration_of_frame(index).unwrap().as_nanos();
        let timing = MediaTiming::new(
            OriginalTimestamp::new(MediaTimestamp::new(pts), time_base),
            NormalizedTimestamp::from_nanos(pts),
            NormalizedDuration::from_nanos(duration).unwrap(),
            domain,
            SequenceNumber::new(index),
        )
        .unwrap()
        .with_flags(flags);
        let payload = CpuVideoPayload::allocate(PixelFormat::Nv12, dimensions).unwrap();
        Arc::new(CpuVideoFrame::new(timing, payload))
    };
    let to_input = |frame: Arc<CpuVideoFrame>| {
        let timing = frame.timing();
        let input = DelayInputFrame::new(
            timing.presentation_timestamp().as_nanos(),
            timing.duration().as_nanos(),
            frame.payload().byte_len(),
            Arc::clone(&frame),
        )
        .unwrap();
        if timing.flags().contains(MediaFlags::DISCONTINUITY) {
            input.with_discontinuity()
        } else {
            input
        }
    };

    let frame_bytes = 64 * 36 * 3 / 2;
    let cfg = config(64, 8 * frame_bytes);
    let mut line = VideoDelayLine::new(cfg, frames(4, NTSC_60)).unwrap();
    for index in 0..120 {
        let frame = make(index, MediaFlags::NONE);
        let now = frame.timing().presentation_timestamp().as_nanos();
        line.push(now, to_input(frame)).unwrap();
        if let DelayOutput::Frame(delayed) = line.output(now).unwrap() {
            assert_eq!(delayed.payload.timing().sequence().get(), index - 4);
        } else {
            assert!(index < 4);
        }
        assert_eq!(
            line.telemetry().bytes_held,
            5.min(usize::try_from(index).unwrap() + 1) * frame_bytes
        );
    }
    let frame = make(120, MediaFlags::DISCONTINUITY);
    let now = frame.timing().presentation_timestamp().as_nanos();
    assert!(line.push(now, to_input(frame)).unwrap().discontinuity);
}
