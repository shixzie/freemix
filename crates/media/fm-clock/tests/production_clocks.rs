//! Acceptance tests for production clocks (SW-013): timers, time of day with
//! explicit time zones, remaining media time, session timers, formatting,
//! and the registry's command/reading API. No test reads a system clock.

use core::num::NonZeroU128;

use fm_clock::production::{
    ClockCommand, ClockKind, ClockName, ClockNameError, ClockValue, ClockValueError, DisplayFormat,
    DurationFormat, MAX_PENDING_TIMER_EVENTS, MAX_PRODUCTION_CLOCKS, ManualWallClock,
    MediaProgress, MediaProgressError, OverrunPolicy, ProductionClock, ProductionClockError,
    ProductionClocks, ReadingState, Rounding, SessionKind, SessionTimer, TimeOfDayFormat,
    TimeReading, TimeZone, Timer, TimerConfig, TimerError, TimerEvent, TimerPhase, UtcInstant,
    WallClock, Weekday, ZoneError, format_duration, format_time_of_day, parse_clock_value,
};
use fm_clock::{Clock as _, ClockDomainId, ClockDuration, ClockTime, FrameCadence, ManualClock};
use fm_types::FrameRate;

const SECOND: i64 = 1_000_000_000;
const MILLI: i64 = 1_000_000;

fn ns(nanos: i64) -> ClockValue {
    ClockValue::from_nanos(nanos).unwrap()
}

fn secs(seconds: i64) -> ClockValue {
    ClockValue::from_secs(seconds).unwrap()
}

fn millis(millis: i64) -> ClockValue {
    ClockValue::from_millis(millis).unwrap()
}

fn at_ms(millis: u64) -> ClockTime {
    ClockTime::from_nanos(millis * 1_000_000)
}

fn at_s(seconds: u64) -> ClockTime {
    ClockTime::from_nanos(seconds * 1_000_000_000)
}

fn rate(numerator: u32, denominator: u32) -> FrameRate {
    FrameRate::new(numerator, denominator).unwrap()
}

fn fmt(value: ClockValue, format: DurationFormat, rounding: Rounding) -> String {
    format_duration(value, format, rounding)
}

fn utc(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> UtcInstant {
    UtcInstant::from_utc_civil(year, month, day, hour, minute, second, 0).unwrap()
}

fn name(text: &str) -> ClockName {
    ClockName::new(text).unwrap()
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

#[test]
fn countdown_rounding_shows_each_second_for_a_full_second_and_zero_at_expiry() {
    let ms = DurationFormat::MinutesSeconds;
    let cases = [
        (10 * SECOND, "00:10"),
        (9 * SECOND + 1, "00:10"),
        (9 * SECOND, "00:09"),
        (1, "00:01"),
        (0, "00:00"),
        (-1, "00:00"),
        (-SECOND + 1, "00:00"),
        (-SECOND, "-00:01"),
        (-SECOND - 500 * MILLI, "-00:01"),
        (-2 * SECOND, "-00:02"),
    ];
    for (nanos, expected) in cases {
        assert_eq!(fmt(ns(nanos), ms, Rounding::Ceil), expected, "{nanos}ns");
    }
}

#[test]
fn count_up_rounding_floors_and_never_wraps_large_units() {
    let cases = [
        (
            59 * SECOND + 999 * MILLI,
            DurationFormat::MinutesSeconds,
            "00:59",
        ),
        (90 * 60 * SECOND, DurationFormat::MinutesSeconds, "90:00"),
        (
            3_600 * SECOND,
            DurationFormat::HoursMinutesSeconds,
            "01:00:00",
        ),
        (
            100 * 3_600 * SECOND,
            DurationFormat::HoursMinutesSeconds,
            "100:00:00",
        ),
        (95 * SECOND, DurationFormat::Seconds, "95"),
        (5 * SECOND, DurationFormat::Compact, "0:05"),
        (
            59 * 60 * SECOND + 59 * SECOND,
            DurationFormat::Compact,
            "59:59",
        ),
        (3_600 * SECOND, DurationFormat::Compact, "1:00:00"),
        (-SECOND / 2, DurationFormat::MinutesSeconds, "-00:01"),
    ];
    for (nanos, format, expected) in cases {
        assert_eq!(
            fmt(ns(nanos), format, Rounding::Floor),
            expected,
            "{nanos}ns"
        );
    }
}

#[test]
fn sub_second_presets_round_at_their_own_unit() {
    let value = ns(SECOND + 250 * MILLI);
    assert_eq!(
        fmt(
            value,
            DurationFormat::HoursMinutesSecondsTenths,
            Rounding::Floor
        ),
        "00:00:01.2"
    );
    assert_eq!(
        fmt(
            value,
            DurationFormat::HoursMinutesSecondsTenths,
            Rounding::Ceil
        ),
        "00:00:01.3"
    );
    assert_eq!(
        fmt(value, DurationFormat::MinutesSecondsTenths, Rounding::Floor),
        "00:01.2"
    );
    assert_eq!(
        fmt(
            ns(SECOND + 1),
            DurationFormat::HoursMinutesSecondsMillis,
            Rounding::Floor
        ),
        "00:00:01.000"
    );
    assert_eq!(
        fmt(
            ns(SECOND + 1),
            DurationFormat::HoursMinutesSecondsMillis,
            Rounding::Ceil
        ),
        "00:00:01.001"
    );
    assert_eq!(
        fmt(
            ns(59_999 * MILLI + 1),
            DurationFormat::MinutesSecondsTenths,
            Rounding::Ceil
        ),
        "01:00.0"
    );
}

#[test]
fn frame_preset_at_integer_rate() {
    let format = DurationFormat::HoursMinutesSecondsFrames(rate(25, 1));
    assert_eq!(
        fmt(ns(SECOND + 500 * MILLI), format, Rounding::Floor),
        "00:00:01:12"
    );
    assert_eq!(fmt(ns(SECOND - 1), format, Rounding::Floor), "00:00:00:24");
    assert_eq!(fmt(ns(SECOND - 1), format, Rounding::Ceil), "00:00:01:00");
    assert_eq!(
        fmt(ns(-SECOND - 40 * MILLI), format, Rounding::Floor),
        "-00:00:01:01"
    );
}

#[test]
fn frame_preset_at_fractional_rate_matches_frame_cadence() {
    let ntsc = rate(30_000, 1_001);
    let format = DurationFormat::HoursMinutesSecondsFrames(ntsc);
    // Frame 29 starts at 0.9676s and frame 30 at 1.001s.
    assert_eq!(fmt(ns(SECOND), format, Rounding::Floor), "00:00:00:29");
    assert_eq!(fmt(ns(SECOND), format, Rounding::Ceil), "00:00:01:00");

    let cadence = FrameCadence::new(ntsc, ClockTime::ZERO);
    let mut previous_label = String::new();
    for frame in (0..200_000_u64).step_by(7) {
        let start = cadence.time_of_frame(frame).unwrap().as_nanos();
        let start = i64::try_from(start).unwrap();
        let label = fmt(ns(start), format, Rounding::Floor);
        // The exact start time of a frame floors and ceils to that frame.
        assert_eq!(
            label,
            fmt(ns(start), format, Rounding::Ceil),
            "frame {frame}"
        );
        // One nanosecond earlier belongs to the previous frame.
        if frame > 0 {
            assert_ne!(label, fmt(ns(start - 1), format, Rounding::Floor));
        }
        // Seconds agree with the plain HH:MM:SS preset of the same instant.
        assert_eq!(
            &label[..8],
            fmt(
                ns(start),
                DurationFormat::HoursMinutesSeconds,
                Rounding::Floor
            ),
            "frame {frame}"
        );
        let frames_in_second: u32 = label[9..].parse().unwrap();
        assert!(frames_in_second < 30, "frame {frame} label {label}");
        assert!(label > previous_label, "labels increase monotonically");
        previous_label = label;
    }
}

#[test]
fn formatting_extremes_do_not_overflow() {
    for format in [
        DurationFormat::HoursMinutesSeconds,
        DurationFormat::MinutesSeconds,
        DurationFormat::Seconds,
        DurationFormat::HoursMinutesSecondsMillis,
        DurationFormat::HoursMinutesSecondsFrames(rate(60_000, 1_001)),
        DurationFormat::HoursMinutesSecondsFrames(rate(u32::MAX, 1)),
        DurationFormat::Compact,
    ] {
        for rounding in [Rounding::Floor, Rounding::Ceil] {
            let opposite = match rounding {
                Rounding::Floor => Rounding::Ceil,
                Rounding::Ceil => Rounding::Floor,
            };
            // floor(-x) = -ceil(x): the negative extreme mirrors the positive
            // extreme rounded the other way.
            let max = fmt(ClockValue::MAX, format, opposite);
            let min = fmt(ClockValue::MIN, format, rounding);
            assert_eq!(min.strip_prefix('-'), Some(max.as_str()), "{format:?}");
        }
    }
    assert_eq!(
        fmt(
            ClockValue::MAX,
            DurationFormat::HoursMinutesSeconds,
            Rounding::Floor
        ),
        "99999:59:59"
    );
    assert_eq!(
        fmt(
            ClockValue::MIN,
            DurationFormat::HoursMinutesSeconds,
            Rounding::Ceil
        ),
        "-99999:59:59"
    );
}

#[test]
fn time_of_day_presets() {
    let zone = TimeZone::utc();
    let noonish = zone.to_local(utc(2026, 10, 14, 12, 34, 56));
    assert_eq!(
        format_time_of_day(&noonish, TimeOfDayFormat::Hours24),
        "12:34:56"
    );
    assert_eq!(
        format_time_of_day(&noonish, TimeOfDayFormat::Hours24Minutes),
        "12:34"
    );
    assert_eq!(
        format_time_of_day(&noonish, TimeOfDayFormat::Hours12),
        "12:34:56 PM"
    );
    assert_eq!(
        format_time_of_day(&noonish, TimeOfDayFormat::Hours12Minutes),
        "12:34 PM"
    );
    assert_eq!(
        format_time_of_day(&noonish, TimeOfDayFormat::Date),
        "2026-10-14"
    );
    assert_eq!(
        format_time_of_day(&noonish, TimeOfDayFormat::Rfc3339),
        "2026-10-14T12:34:56+00:00"
    );
    assert_eq!(noonish.weekday, Weekday::Wednesday);

    let midnight = zone.to_local(utc(2026, 1, 1, 0, 0, 0));
    assert_eq!(
        format_time_of_day(&midnight, TimeOfDayFormat::Hours12),
        "12:00:00 AM"
    );
    let evening = zone.to_local(utc(2026, 1, 1, 23, 5, 0));
    assert_eq!(
        format_time_of_day(&evening, TimeOfDayFormat::Hours12Minutes),
        "11:05 PM"
    );

    // Sub-second parts truncate; they never round the displayed second up.
    let almost = UtcInstant::new(59, 999_999_999).unwrap();
    assert_eq!(
        format_time_of_day(&zone.to_local(almost), TimeOfDayFormat::Hours24),
        "00:00:59"
    );
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[test]
fn clock_values_parse_strictly() {
    let accepted = [
        ("0", 0),
        ("-0", 0),
        ("90", 90 * SECOND),
        ("1:30", 90 * SECOND),
        ("90:00", 90 * 60 * SECOND),
        ("01:00:00", 3_600 * SECOND),
        ("-00:10", -10 * SECOND),
        ("1:02:03.5", 3_723 * SECOND + 500 * MILLI),
        ("0.000000001", 1),
        ("99999:59:59.999999999", ClockValue::MAX.as_nanos()),
        ("-99999:59:59.999999999", ClockValue::MIN.as_nanos()),
    ];
    for (text, nanos) in accepted {
        assert_eq!(parse_clock_value(text), Ok(ns(nanos)), "{text}");
    }
    let rejected = [
        "",
        "-",
        " 1",
        "1 ",
        "+1",
        "1:2",
        "1:60",
        "1:00:60",
        "1:00:00:00",
        ":30",
        "1::00",
        "1.",
        ".5",
        "1.1234567890",
        "1:3a",
        "1,5",
        "--1",
        "1:-30",
        "١",
    ];
    for text in rejected {
        assert!(
            matches!(parse_clock_value(text), Err(ClockValueError::Syntax(_))),
            "{text:?} should be a syntax error"
        );
    }
    assert_eq!(
        parse_clock_value("100000:00:00"),
        Err(ClockValueError::OutOfRange)
    );
    assert_eq!(
        parse_clock_value("9999999999999"),
        Err(ClockValueError::OutOfRange)
    );
}

#[test]
fn formatted_values_parse_back() {
    for nanos in [0, SECOND, 59 * SECOND, 3_661 * SECOND, -754 * SECOND] {
        let text = fmt(
            ns(nanos),
            DurationFormat::HoursMinutesSeconds,
            Rounding::Floor,
        );
        assert_eq!(parse_clock_value(&text), Ok(ns(nanos)));
        let millis_text = fmt(
            ns(nanos + 123 * MILLI),
            DurationFormat::HoursMinutesSecondsMillis,
            Rounding::Floor,
        );
        if nanos >= 0 {
            assert_eq!(parse_clock_value(&millis_text), Ok(ns(nanos + 123 * MILLI)));
        }
    }
}

// ---------------------------------------------------------------------------
// Timers
// ---------------------------------------------------------------------------

fn countdown(seconds: i64, overrun: OverrunPolicy) -> Timer {
    Timer::new(TimerConfig::countdown(secs(seconds), overrun)).unwrap()
}

#[test]
fn timer_configuration_is_validated() {
    assert_eq!(
        Timer::new(TimerConfig::countdown(secs(-1), OverrunPolicy::Continue)),
        Err(TimerError::NegativePreset)
    );
    let mut with_limit = TimerConfig::countdown(secs(5), OverrunPolicy::Continue);
    with_limit.limit = Some(secs(10));
    assert_eq!(Timer::new(with_limit), Err(TimerError::LimitOnCountdown));
    assert_eq!(
        Timer::new(TimerConfig::count_up_to(
            secs(5),
            secs(5),
            OverrunPolicy::Continue
        )),
        Err(TimerError::LimitNotAbovePreset)
    );
    assert!(Timer::new(TimerConfig::stopwatch()).is_ok());
}

#[test]
fn stopping_countdown_expires_exactly_once_at_zero() {
    let mut timer = countdown(10, OverrunPolicy::StopAtTarget);
    let idle = timer.snapshot(at_s(0));
    assert_eq!(
        (idle.phase, idle.value, idle.expired),
        (TimerPhase::Idle, secs(10), false)
    );

    timer.start(at_s(100)).unwrap();
    let running = timer.snapshot(at_s(105));
    assert_eq!(
        (running.phase, running.value),
        (TimerPhase::Running, secs(5))
    );
    assert_eq!(timer.snapshot(at_ms(109_999)).value, millis(1));
    assert!(!timer.snapshot(at_ms(109_999)).expired);

    let expired = timer.snapshot(at_s(110));
    assert_eq!(expired.phase, TimerPhase::Expired);
    assert_eq!(expired.value, ClockValue::ZERO);
    assert_eq!(expired.expired_at, Some(at_s(110)));
    // Reading is pure; long after expiry the value stays pinned at zero.
    assert_eq!(timer.snapshot(at_s(500)).value, ClockValue::ZERO);

    assert_eq!(
        timer.poll(at_s(200)).unwrap(),
        vec![TimerEvent::Expired { at: at_s(110) }]
    );
    assert_eq!(timer.poll(at_s(201)).unwrap(), vec![]);
    assert_eq!(timer.start(at_s(202)), Err(TimerError::Expired));

    timer.reset(at_s(203)).unwrap();
    let reset = timer.snapshot(at_s(204));
    assert_eq!(
        (reset.phase, reset.value, reset.expired),
        (TimerPhase::Idle, secs(10), false)
    );
    assert_eq!(reset.expired_at, None);
}

#[test]
fn overrunning_countdown_goes_negative_and_reports_once() {
    let mut timer = countdown(10, OverrunPolicy::Continue);
    timer.start(at_s(100)).unwrap();
    let late = timer.snapshot(at_ms(112_300));
    assert_eq!(late.phase, TimerPhase::Running);
    assert_eq!(late.value, millis(-2_300));
    assert!(late.expired);
    assert_eq!(late.expired_at, Some(at_s(110)));
    assert_eq!(
        format_duration(late.value, DurationFormat::MinutesSeconds, Rounding::Ceil),
        "-00:02"
    );
    assert_eq!(
        timer.poll(at_s(113)).unwrap(),
        vec![TimerEvent::Expired { at: at_s(110) }]
    );
    assert_eq!(timer.poll(at_s(150)).unwrap(), vec![]);
    assert!(timer.snapshot(at_s(150)).expired);
}

#[test]
fn pause_and_resume_exclude_paused_time() {
    let mut timer = countdown(10, OverrunPolicy::StopAtTarget);
    timer.pause(at_s(0)).unwrap(); // pausing an idle timer is a no-op
    assert_eq!(timer.snapshot(at_s(0)).phase, TimerPhase::Idle);

    timer.start(at_s(0)).unwrap();
    timer.pause(at_s(3)).unwrap();
    timer.pause(at_s(4)).unwrap(); // idempotent
    let paused = timer.snapshot(at_s(50));
    assert_eq!((paused.phase, paused.value), (TimerPhase::Paused, secs(7)));

    timer.start(at_s(50)).unwrap();
    assert_eq!(timer.snapshot(at_s(52)).value, secs(5));
    // Starting a running timer does not re-anchor it.
    timer.start(at_s(53)).unwrap();
    assert_eq!(timer.snapshot(at_s(54)).value, secs(3));
    assert_eq!(timer.snapshot(at_s(57)).expired_at, Some(at_s(57)));
}

#[test]
fn many_pause_cycles_accumulate_without_drift() {
    let mut timer = Timer::new(TimerConfig::stopwatch()).unwrap();
    let mut now = 1_u64;
    for _ in 0..10_000 {
        timer.start(ClockTime::from_nanos(now)).unwrap();
        now += 16_683_333; // one 59.94 Hz frame, truncated
        timer.pause(ClockTime::from_nanos(now)).unwrap();
        now += 1_001;
    }
    assert_eq!(
        timer.snapshot(ClockTime::from_nanos(now)).value,
        ns(10_000 * 16_683_333)
    );
}

#[test]
fn operations_reject_time_regression_without_side_effects() {
    let mut timer = countdown(10, OverrunPolicy::Continue);
    timer.start(at_s(10)).unwrap();
    let before = timer.clone();
    assert_eq!(
        timer.pause(at_s(9)),
        Err(TimerError::TimeRegression {
            last: at_s(10),
            attempted: at_s(9)
        })
    );
    assert!(matches!(
        timer.set_value(at_s(9), secs(1)),
        Err(TimerError::TimeRegression { .. })
    ));
    assert!(matches!(
        timer.adjust(at_s(9), secs(1)),
        Err(TimerError::TimeRegression { .. })
    ));
    assert!(matches!(
        timer.set_preset(at_s(9), secs(1)),
        Err(TimerError::TimeRegression { .. })
    ));
    assert!(matches!(
        timer.poll(at_s(9)),
        Err(TimerError::TimeRegression { .. })
    ));
    assert_eq!(timer, before);
    // Reads with an earlier time are total and read as the anchor.
    assert_eq!(timer.snapshot(at_s(1)).value, secs(10));
    // The same instant is not a regression.
    timer.pause(at_s(10)).unwrap();
}

#[test]
fn set_value_keeps_run_state_and_rearms_expiry() {
    let mut timer = countdown(10, OverrunPolicy::Continue);
    timer.start(at_s(0)).unwrap();
    // Cross zero at 10s without polling.
    timer.set_value(at_s(12), secs(5)).unwrap();
    let rearmed = timer.snapshot(at_s(12));
    assert_eq!(
        (rearmed.phase, rearmed.value, rearmed.expired),
        (TimerPhase::Running, secs(5), false)
    );
    // Cross zero again at 17s; both expiries are delivered in order.
    assert_eq!(
        timer.poll(at_s(20)).unwrap(),
        vec![
            TimerEvent::Expired { at: at_s(10) },
            TimerEvent::Expired { at: at_s(17) }
        ]
    );

    // Setting a paused timer keeps it paused.
    timer.pause(at_s(21)).unwrap();
    timer.set_value(at_s(22), secs(30)).unwrap();
    assert_eq!(timer.snapshot(at_s(99)).phase, TimerPhase::Paused);
    assert_eq!(timer.snapshot(at_s(99)).value, secs(30));

    // Setting an idle timer parks it paused at the value.
    let mut idle = countdown(10, OverrunPolicy::Continue);
    idle.set_value(at_s(0), secs(4)).unwrap();
    assert_eq!(idle.snapshot(at_s(9)).phase, TimerPhase::Paused);
    assert_eq!(idle.snapshot(at_s(9)).value, secs(4));
}

#[test]
fn set_value_onto_the_target_expires_at_the_operation_time() {
    let mut timer = countdown(10, OverrunPolicy::StopAtTarget);
    timer.start(at_s(0)).unwrap();
    timer.pause(at_s(2)).unwrap();
    timer.set_value(at_s(3), ClockValue::ZERO).unwrap();
    let snapshot = timer.snapshot(at_s(4));
    assert_eq!(
        (snapshot.phase, snapshot.expired),
        (TimerPhase::Expired, true)
    );
    assert_eq!(snapshot.expired_at, Some(at_s(3)));
    assert_eq!(
        timer.poll(at_s(4)).unwrap(),
        vec![TimerEvent::Expired { at: at_s(3) }]
    );
    // Setting a value before the target recovers an expired timer, paused.
    timer.set_value(at_s(5), secs(2)).unwrap();
    assert_eq!(timer.snapshot(at_s(5)).phase, TimerPhase::Paused);
    timer.start(at_s(6)).unwrap();
    assert_eq!(timer.snapshot(at_s(8)).expired_at, Some(at_s(8)));
}

#[test]
fn set_value_range_depends_on_direction_and_policy() {
    let mut stopping = countdown(10, OverrunPolicy::StopAtTarget);
    assert_eq!(
        stopping.set_value(at_s(0), secs(-1)),
        Err(TimerError::ValueOutOfRange)
    );
    let mut overrunning = countdown(10, OverrunPolicy::Continue);
    overrunning.set_value(at_s(0), secs(-1)).unwrap();
    assert!(overrunning.snapshot(at_s(0)).expired);

    let mut up = Timer::new(TimerConfig::count_up_to(
        ClockValue::ZERO,
        secs(60),
        OverrunPolicy::StopAtTarget,
    ))
    .unwrap();
    assert_eq!(
        up.set_value(at_s(0), secs(61)),
        Err(TimerError::ValueOutOfRange)
    );
    assert_eq!(
        up.set_value(at_s(0), secs(-1)),
        Err(TimerError::ValueOutOfRange)
    );
    let mut up_overrun = Timer::new(TimerConfig::count_up_to(
        ClockValue::ZERO,
        secs(60),
        OverrunPolicy::Continue,
    ))
    .unwrap();
    up_overrun.set_value(at_s(0), secs(61)).unwrap();
    assert!(up_overrun.snapshot(at_s(0)).expired);
}

#[test]
fn adjust_adds_time_and_clamps_into_range() {
    let mut timer = countdown(10, OverrunPolicy::StopAtTarget);
    timer.start(at_s(0)).unwrap();
    timer.adjust(at_s(7), secs(30)).unwrap();
    assert_eq!(timer.snapshot(at_s(7)).value, secs(33));
    timer.adjust(at_s(8), secs(-100)).unwrap();
    let snapshot = timer.snapshot(at_s(8));
    assert_eq!(
        (snapshot.phase, snapshot.value),
        (TimerPhase::Expired, ClockValue::ZERO)
    );
    assert_eq!(snapshot.expired_at, Some(at_s(8)));

    let mut overrun = countdown(10, OverrunPolicy::Continue);
    overrun.adjust(at_s(0), ClockValue::MIN).unwrap();
    overrun.adjust(at_s(0), ClockValue::MIN).unwrap();
    assert_eq!(overrun.snapshot(at_s(0)).value, ClockValue::MIN);
}

#[test]
fn set_preset_resets_and_rejects_invalid_presets() {
    let mut timer = Timer::new(TimerConfig::count_up_to(
        ClockValue::ZERO,
        secs(60),
        OverrunPolicy::StopAtTarget,
    ))
    .unwrap();
    timer.start(at_s(0)).unwrap();
    let before = timer.clone();
    assert_eq!(
        timer.set_preset(at_s(1), secs(60)),
        Err(TimerError::LimitNotAbovePreset)
    );
    assert_eq!(
        timer.set_preset(at_s(1), secs(-1)),
        Err(TimerError::NegativePreset)
    );
    assert_eq!(timer, before);
    timer.set_preset(at_s(2), secs(30)).unwrap();
    let snapshot = timer.snapshot(at_s(3));
    assert_eq!(
        (snapshot.phase, snapshot.value),
        (TimerPhase::Idle, secs(30))
    );
    assert_eq!(timer.config().preset, secs(30));
}

#[test]
fn count_up_to_limit_stops_or_overruns() {
    let mut stop = Timer::new(TimerConfig::count_up_to(
        secs(1),
        secs(5),
        OverrunPolicy::StopAtTarget,
    ))
    .unwrap();
    stop.start(at_s(10)).unwrap();
    assert_eq!(stop.snapshot(at_ms(13_999)).value, millis(4_999));
    let done = stop.snapshot(at_s(30));
    assert_eq!((done.phase, done.value), (TimerPhase::Expired, secs(5)));
    assert_eq!(done.expired_at, Some(at_s(14)));

    let mut stopwatch = Timer::new(TimerConfig::stopwatch()).unwrap();
    stopwatch.start(at_s(0)).unwrap();
    assert_eq!(stopwatch.poll(at_s(1_000_000)).unwrap(), vec![]);
    assert!(!stopwatch.snapshot(at_s(1_000_000)).expired);
}

#[test]
fn expiry_time_is_exact_to_the_nanosecond() {
    let mut timer = Timer::new(TimerConfig::countdown(ns(1), OverrunPolicy::StopAtTarget)).unwrap();
    timer.start(ClockTime::from_nanos(1_000_000_007)).unwrap();
    assert!(!timer.snapshot(ClockTime::from_nanos(1_000_000_007)).expired);
    assert_eq!(
        timer
            .snapshot(ClockTime::from_nanos(1_000_000_008))
            .expired_at,
        Some(ClockTime::from_nanos(1_000_000_008))
    );

    // A zero-preset countdown expires the instant it starts.
    let mut zero = countdown(0, OverrunPolicy::Continue);
    zero.start(at_s(5)).unwrap();
    assert_eq!(
        zero.poll(at_s(5)).unwrap(),
        vec![TimerEvent::Expired { at: at_s(5) }]
    );
}

#[test]
fn values_saturate_at_the_supported_range() {
    let mut timer = Timer::new(TimerConfig::stopwatch()).unwrap();
    timer
        .set_value(at_s(0), ClockValue::MAX.saturating_sub(ns(1)))
        .unwrap();
    timer.start(at_s(0)).unwrap();
    assert_eq!(
        timer.snapshot(ClockTime::from_nanos(u64::MAX)).value,
        ClockValue::MAX
    );

    let mut overrun = countdown(1, OverrunPolicy::Continue);
    overrun.start(ClockTime::ZERO).unwrap();
    // Elapsed time saturates at ClockValue::MAX before it is subtracted.
    assert_eq!(
        overrun.snapshot(ClockTime::from_nanos(u64::MAX)).value,
        secs(1).saturating_sub(ClockValue::MAX)
    );
}

#[test]
fn pending_events_are_bounded() {
    let mut timer = countdown(1, OverrunPolicy::Continue);
    timer.start(at_s(0)).unwrap();
    let mut now = 0;
    for _ in 0..(MAX_PENDING_TIMER_EVENTS + 3) {
        now += 2;
        timer.set_value(at_s(now), secs(1)).unwrap();
    }
    let events = timer.poll(at_s(now + 2)).unwrap();
    assert_eq!(events.len(), MAX_PENDING_TIMER_EVENTS);
    // The newest events are kept.
    assert_eq!(
        events.last(),
        Some(&TimerEvent::Expired { at: at_s(now + 1) })
    );
}

// ---------------------------------------------------------------------------
// Time zones and time of day
// ---------------------------------------------------------------------------

fn assert_local(zone: &TimeZone, instant: UtcInstant, expected: &str, abbreviation: &str) {
    let local = zone.to_local(instant);
    assert_eq!(
        format_time_of_day(&local, TimeOfDayFormat::Rfc3339),
        expected,
        "{zone} at {instant:?}"
    );
    assert_eq!(zone.resolve(instant).abbreviation, abbreviation);
}

#[test]
fn fixed_offsets_including_fractional_and_negative() {
    let india = TimeZone::parse_posix("IST-5:30").unwrap();
    assert_eq!(india.utc_offset_seconds(), 19_800);
    assert_local(
        &india,
        utc(2026, 1, 1, 0, 0, 0),
        "2026-01-01T05:30:00+05:30",
        "IST",
    );

    let nepal = TimeZone::parse_posix("<+0545>-5:45").unwrap();
    assert_local(
        &nepal,
        utc(2026, 6, 1, 18, 15, 0),
        "2026-06-02T00:00:00+05:45",
        "+0545",
    );

    let brazil = TimeZone::parse_posix("<-03>3").unwrap();
    assert_local(
        &brazil,
        utc(2026, 1, 1, 1, 0, 0),
        "2025-12-31T22:00:00-03:00",
        "-03",
    );

    let fixed = TimeZone::fixed("Z", 0).unwrap();
    assert_eq!(
        fixed.resolve(utc(2026, 1, 1, 0, 0, 0)).utc_offset_seconds,
        0
    );
    assert_eq!(
        TimeZone::fixed("BAD ZONE", 0),
        Err(ZoneError::InvalidAbbreviation)
    );
    assert_eq!(
        TimeZone::fixed("X", 25 * 3_600),
        Err(ZoneError::OffsetOutOfRange)
    );
}

#[test]
fn us_eastern_transitions_at_the_exact_second() {
    let eastern = TimeZone::parse_posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
    // 2026-03-08 02:00 EST = 07:00Z.
    assert_local(
        &eastern,
        utc(2026, 3, 8, 6, 59, 59),
        "2026-03-08T01:59:59-05:00",
        "EST",
    );
    assert_local(
        &eastern,
        utc(2026, 3, 8, 7, 0, 0),
        "2026-03-08T03:00:00-04:00",
        "EDT",
    );
    // 2026-11-01 02:00 EDT = 06:00Z; 01:xx happens twice.
    assert_local(
        &eastern,
        utc(2026, 11, 1, 5, 59, 59),
        "2026-11-01T01:59:59-04:00",
        "EDT",
    );
    assert_local(
        &eastern,
        utc(2026, 11, 1, 6, 0, 0),
        "2026-11-01T01:00:00-05:00",
        "EST",
    );
    assert!(eastern.resolve(utc(2026, 7, 4, 12, 0, 0)).is_dst);
    assert!(!eastern.resolve(utc(2026, 12, 25, 12, 0, 0)).is_dst);
    // Year boundary in local standard time.
    assert_local(
        &eastern,
        utc(2027, 1, 1, 4, 59, 59),
        "2026-12-31T23:59:59-05:00",
        "EST",
    );
}

#[test]
fn central_europe_transitions_on_the_last_sunday() {
    let europe = TimeZone::parse_posix("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
    assert_local(
        &europe,
        utc(2026, 3, 29, 0, 59, 59),
        "2026-03-29T01:59:59+01:00",
        "CET",
    );
    assert_local(
        &europe,
        utc(2026, 3, 29, 1, 0, 0),
        "2026-03-29T03:00:00+02:00",
        "CEST",
    );
    assert_local(
        &europe,
        utc(2026, 10, 25, 0, 59, 59),
        "2026-10-25T02:59:59+02:00",
        "CEST",
    );
    assert_local(
        &europe,
        utc(2026, 10, 25, 1, 0, 0),
        "2026-10-25T02:00:00+01:00",
        "CET",
    );
}

#[test]
fn southern_hemisphere_daylight_spans_the_new_year() {
    let sydney = TimeZone::parse_posix("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
    assert_local(
        &sydney,
        utc(2026, 1, 15, 0, 0, 0),
        "2026-01-15T11:00:00+11:00",
        "AEDT",
    );
    assert_local(
        &sydney,
        utc(2026, 7, 1, 0, 0, 0),
        "2026-07-01T10:00:00+10:00",
        "AEST",
    );
    // Ends 2026-04-05 03:00 AEDT = 04-04 16:00Z.
    assert_local(
        &sydney,
        utc(2026, 4, 4, 15, 59, 59),
        "2026-04-05T02:59:59+11:00",
        "AEDT",
    );
    assert_local(
        &sydney,
        utc(2026, 4, 4, 16, 0, 0),
        "2026-04-05T02:00:00+10:00",
        "AEST",
    );
    // Starts 2026-10-04 02:00 AEST = 10-03 16:00Z.
    assert_local(
        &sydney,
        utc(2026, 10, 3, 15, 59, 59),
        "2026-10-04T01:59:59+10:00",
        "AEST",
    );
    assert_local(
        &sydney,
        utc(2026, 10, 3, 16, 0, 0),
        "2026-10-04T03:00:00+11:00",
        "AEDT",
    );
    // New Year's Eve in daylight time crosses into the next local year.
    assert_local(
        &sydney,
        utc(2026, 12, 31, 13, 0, 0),
        "2027-01-01T00:00:00+11:00",
        "AEDT",
    );
}

#[test]
fn permanent_daylight_rule_from_rfc_8536() {
    let zone = TimeZone::parse_posix("EST5EDT4,0/0,J365/25").unwrap();
    for instant in [
        utc(2026, 1, 1, 5, 0, 0),
        utc(2026, 6, 1, 0, 0, 0),
        utc(2026, 12, 31, 23, 59, 59),
        utc(2028, 2, 29, 12, 0, 0),
    ] {
        assert!(zone.resolve(instant).is_dst, "{instant:?}");
    }
}

#[test]
fn posix_parse_errors_are_specific() {
    let cases: [(&str, ZoneError); 12] = [
        ("", ZoneError::InvalidAbbreviation),
        ("EST", ZoneError::Syntax("expected hours")),
        ("ES5", ZoneError::InvalidAbbreviation),
        ("EST5EDT", ZoneError::MissingRule),
        ("EST25", ZoneError::OffsetOutOfRange),
        ("EST5EDT,M13.1.0,M11.1.0", ZoneError::InvalidTransitionDate),
        (
            "EST5EDT,M3.2.0/168,M11.1.0",
            ZoneError::TransitionTimeOutOfRange,
        ),
        (
            "EST5EDT,M3.2.0",
            ZoneError::Syntax("expected ',' before the daylight end rule"),
        ),
        (
            "<+05-5",
            ZoneError::Syntax("unterminated quoted abbreviation"),
        ),
        (
            "AAA0BBB0,M3.2.0,M11.1.0",
            ZoneError::DegenerateDaylightOffset,
        ),
        (
            "EST5:7",
            ZoneError::Syntax("expected two-digit minutes or seconds"),
        ),
        (
            "EST5:60",
            ZoneError::Syntax("minutes and seconds must be below 60"),
        ),
    ];
    for (text, error) in cases {
        assert_eq!(TimeZone::parse_posix(text), Err(error), "{text:?}");
    }
    assert!(TimeZone::parse_posix("EST5EDT,M3.2.0,M11.1.0x").is_err());
    assert!(TimeZone::parse_posix("EST5 ").is_err());
}

#[test]
fn posix_text_round_trips() {
    for (input, canonical) in [
        ("UTC0", "UTC0"),
        ("EST5EDT,M3.2.0,M11.1.0", "EST5EDT,M3.2.0,M11.1.0"),
        ("EST+5EDT,M3.2.0/2,M11.1.0/02:00", "EST5EDT,M3.2.0,M11.1.0"),
        ("CET-1CEST,M3.5.0,M10.5.0/3", "CET-1CEST,M3.5.0,M10.5.0/3"),
        ("<+0545>-5:45", "<+0545>-5:45"),
        ("EST5EDT4,0/0,J365/25", "EST5EDT,0/0,J365/25"),
        (
            "NZST-12NZDT-13,M9.5.0,M4.1.0/3",
            "NZST-12NZDT,M9.5.0,M4.1.0/3",
        ),
        ("XXX3YYY1,J60/-1:30,300/167", "XXX3YYY1,J60/-1:30,300/167"),
    ] {
        let zone = TimeZone::parse_posix(input).unwrap();
        assert_eq!(zone.to_string(), canonical, "{input}");
        assert_eq!(TimeZone::parse_posix(canonical).unwrap(), zone, "{input}");
    }
}

#[test]
fn utc_instants_are_range_checked() {
    assert!(UtcInstant::from_utc_civil(1, 1, 1, 0, 0, 0, 0).is_ok());
    assert!(UtcInstant::from_utc_civil(9_999, 12, 31, 23, 59, 59, 999_999_999).is_ok());
    assert!(UtcInstant::from_utc_civil(10_000, 1, 1, 0, 0, 0, 0).is_err());
    assert!(UtcInstant::from_utc_civil(2026, 2, 29, 0, 0, 0, 0).is_err());
    assert!(UtcInstant::from_utc_civil(2028, 2, 29, 0, 0, 0, 0).is_ok());
    assert!(UtcInstant::new(0, 1_000_000_000).is_err());
    let before_epoch = UtcInstant::from_unix_nanos(-1).unwrap();
    assert_eq!(
        (before_epoch.unix_seconds(), before_epoch.subsec_nanos()),
        (-1, 999_999_999)
    );
    assert_eq!(
        format_time_of_day(&before_epoch.to_civil(0), TimeOfDayFormat::Rfc3339),
        "1969-12-31T23:59:59+00:00"
    );
}

// ---------------------------------------------------------------------------
// Media and sessions
// ---------------------------------------------------------------------------

#[test]
fn media_remaining_clamps_into_the_play_range() {
    let progress = MediaProgress::new(3_000_000_000, 10_000_000_000).unwrap();
    assert_eq!(progress.remaining(), secs(7));
    assert_eq!(progress.elapsed(), secs(3));
    assert!(!progress.is_finished());

    let past_end = MediaProgress::new(11_000_000_000, 10_000_000_000).unwrap();
    assert_eq!(
        (past_end.remaining(), past_end.is_finished()),
        (ClockValue::ZERO, true)
    );

    let ranged =
        MediaProgress::with_range(1_000_000_000, 10_000_000_000, 2_000_000_000, 8_000_000_000)
            .unwrap();
    assert_eq!(ranged.remaining(), secs(6));
    assert_eq!(ranged.elapsed(), ClockValue::ZERO);
    assert_eq!(ranged.length(), secs(6));

    for (mark_in, mark_out) in [(5, 5), (6, 5), (0, 11)] {
        assert_eq!(
            MediaProgress::with_range(0, 10, mark_in, mark_out),
            Err(MediaProgressError::InvalidRange)
        );
    }
    let empty = MediaProgress::new(0, 0).unwrap();
    assert!(empty.is_finished());
    assert_eq!(
        MediaProgress::new(0, u64::MAX),
        Err(MediaProgressError::OutOfRange)
    );
}

#[test]
fn media_from_frames_uses_exact_frame_start_times() {
    let progress = MediaProgress::from_frames(25, 250, rate(25, 1)).unwrap();
    assert_eq!(progress.remaining(), secs(9));
    let ntsc = MediaProgress::from_frames(0, 30, rate(30_000, 1_001)).unwrap();
    assert_eq!(ntsc.remaining(), ns(1_001_000_000));
    let near_end = MediaProgress::new(9_999_999_999, 10_000_000_000).unwrap();
    assert_eq!(
        format_duration(
            near_end.remaining(),
            DurationFormat::MinutesSeconds,
            Rounding::Ceil
        ),
        "00:01"
    );
}

#[test]
fn session_timer_tracks_session_and_total_across_pauses_and_restarts() {
    let mut session = SessionTimer::new();
    assert!(!session.snapshot(at_s(0)).active);
    session.start(at_s(10)).unwrap();
    assert_eq!(session.snapshot(at_s(70)).session, secs(60));
    session.pause(at_s(70)).unwrap();
    let paused = session.snapshot(at_s(90));
    assert!(paused.active && paused.paused);
    assert_eq!(paused.session, secs(60));
    session.resume(at_s(100)).unwrap();
    session.start(at_s(110)).unwrap(); // repeated "started" is ignored
    assert_eq!(session.snapshot(at_s(130)).session, secs(90));
    session.stop(at_s(130)).unwrap();
    let stopped = session.snapshot(at_s(190));
    assert!(!stopped.active);
    assert_eq!(
        (stopped.session, stopped.total, stopped.sessions),
        (secs(90), secs(90), 1)
    );

    session.start(at_s(200)).unwrap();
    let second = session.snapshot(at_s(210));
    assert_eq!(
        (second.session, second.total, second.sessions),
        (secs(10), secs(100), 2)
    );

    assert!(session.stop(at_s(199)).is_err());
    session.clear(at_s(220)).unwrap();
    let cleared = session.snapshot(at_s(225));
    assert_eq!(
        (cleared.session, cleared.total, cleared.sessions),
        (secs(5), secs(5), 1)
    );
    // Pausing a stopped output does not resurrect it.
    session.stop(at_s(230)).unwrap();
    session.resume(at_s(231)).unwrap();
    session.pause(at_s(232)).unwrap();
    assert!(!session.snapshot(at_s(240)).active);
}

// ---------------------------------------------------------------------------
// Registry and API exposure
// ---------------------------------------------------------------------------

fn reading_at(monotonic_s: u64, instant: UtcInstant) -> TimeReading {
    TimeReading::new(at_s(monotonic_s), instant)
}

fn studio() -> ProductionClocks {
    let mut clocks = ProductionClocks::new();
    clocks
        .insert(
            name("show"),
            ProductionClock::timer(
                TimerConfig::countdown(secs(300), OverrunPolicy::Continue),
                DurationFormat::MinutesSeconds,
            )
            .unwrap(),
        )
        .unwrap();
    clocks
        .insert(
            name("wall"),
            ProductionClock::time_of_day(
                TimeZone::parse_posix("EST5EDT,M3.2.0,M11.1.0").unwrap(),
                TimeOfDayFormat::Hours24,
            ),
        )
        .unwrap();
    clocks
        .insert(
            name("vt"),
            ProductionClock::media_remaining(DurationFormat::MinutesSeconds),
        )
        .unwrap();
    clocks
        .insert(
            name("stream-1"),
            ProductionClock::session(SessionKind::Stream, DurationFormat::HoursMinutesSeconds),
        )
        .unwrap();
    clocks
}

#[test]
fn clock_names_are_validated() {
    assert_eq!(ClockName::new(""), Err(ClockNameError::Empty));
    assert_eq!(
        ClockName::new(&"a".repeat(65)),
        Err(ClockNameError::TooLong)
    );
    assert!(ClockName::new(&"a".repeat(64)).is_ok());
    for invalid in ["a.b", "a b", "é", "a/b"] {
        assert_eq!(
            ClockName::new(invalid),
            Err(ClockNameError::InvalidCharacter)
        );
    }
}

#[test]
fn registry_reports_every_kind_in_name_order() {
    let mut clocks = studio();
    let instant = utc(2026, 7, 4, 16, 30, 5);
    clocks
        .apply(&name("show"), ClockCommand::Start, at_s(0))
        .unwrap();
    clocks
        .apply(&name("stream-1"), ClockCommand::SessionStarted, at_s(0))
        .unwrap();
    clocks
        .apply(
            &name("vt"),
            ClockCommand::UpdateMedia(Some(MediaProgress::new(1, 61_000_000_000).unwrap())),
            at_s(1),
        )
        .unwrap();

    let readings = clocks.readings(reading_at(65, instant));
    let summary: Vec<_> = readings
        .iter()
        .map(|reading| {
            (
                reading.name.as_str(),
                reading.kind,
                reading.state,
                reading.text.as_str(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("show", ClockKind::Timer, ReadingState::Running, "03:55"),
            (
                "stream-1",
                ClockKind::Session(SessionKind::Stream),
                ReadingState::Running,
                "00:01:05"
            ),
            ("vt", ClockKind::MediaRemaining, ReadingState::Live, "01:01"),
            ("wall", ClockKind::TimeOfDay, ReadingState::Live, "12:30:05"),
        ]
    );
}

#[test]
fn binding_values_are_flat_and_stable() {
    let mut clocks = studio();
    clocks
        .apply(&name("show"), ClockCommand::Start, at_s(0))
        .unwrap();
    let at = reading_at(301, utc(2026, 7, 4, 16, 30, 5));
    let show = clocks.reading(&name("show"), at).unwrap();
    assert_eq!(
        show.binding_values(),
        vec![
            ("show".to_owned(), "-00:01".to_owned()),
            ("show.kind".to_owned(), "timer".to_owned()),
            ("show.state".to_owned(), "running".to_owned()),
            ("show.ms".to_owned(), "-1000".to_owned()),
            ("show.expired".to_owned(), "true".to_owned()),
        ]
    );
    let wall = clocks.reading(&name("wall"), at).unwrap();
    assert_eq!(
        wall.binding_values(),
        vec![
            ("wall".to_owned(), "12:30:05".to_owned()),
            ("wall.kind".to_owned(), "time_of_day".to_owned()),
            ("wall.state".to_owned(), "live".to_owned()),
            ("wall.ms".to_owned(), String::new()),
            ("wall.expired".to_owned(), "false".to_owned()),
            ("wall.zone".to_owned(), "EDT".to_owned()),
            ("wall.utc_offset".to_owned(), "-04:00".to_owned()),
        ]
    );
    let vt = clocks.reading(&name("vt"), at).unwrap();
    assert_eq!(
        (vt.state, vt.text.as_str()),
        (ReadingState::Unavailable, "--:--")
    );
    let stream = clocks.reading(&name("stream-1"), at).unwrap();
    assert_eq!(
        stream.binding_values().last(),
        Some(&("stream-1.total_ms".to_owned(), "0".to_owned()))
    );
    assert_eq!(stream.state, ReadingState::Stopped);
    assert!(clocks.reading(&name("missing"), at).is_none());
}

#[test]
fn registry_commands_are_kind_checked_and_atomic() {
    let mut clocks = studio();
    assert_eq!(
        clocks.apply(&name("wall"), ClockCommand::Start, at_s(0)),
        Err(ProductionClockError::UnsupportedCommand {
            kind: ClockKind::TimeOfDay,
            command: "start"
        })
    );
    assert_eq!(
        clocks.apply(&name("show"), ClockCommand::SessionStarted, at_s(0)),
        Err(ProductionClockError::UnsupportedCommand {
            kind: ClockKind::Timer,
            command: "session_started"
        })
    );
    assert_eq!(
        clocks.apply(
            &name("wall"),
            ClockCommand::SetFormat(DisplayFormat::Duration(DurationFormat::Seconds)),
            at_s(0)
        ),
        Err(ProductionClockError::FormatMismatch)
    );
    assert_eq!(
        clocks.apply(&name("nope"), ClockCommand::Start, at_s(0)),
        Err(ProductionClockError::UnknownClock(name("nope")))
    );
    let before = clocks.get(&name("show")).cloned();
    assert_eq!(
        clocks.apply(&name("show"), ClockCommand::SetPreset(secs(-5)), at_s(0)),
        Err(ProductionClockError::Timer(TimerError::NegativePreset))
    );
    assert_eq!(clocks.get(&name("show")).cloned(), before);

    clocks
        .apply(
            &name("show"),
            ClockCommand::SetFormat(DisplayFormat::Duration(DurationFormat::Seconds)),
            at_s(0),
        )
        .unwrap();
    clocks
        .apply(
            &name("wall"),
            ClockCommand::SetTimeZone(TimeZone::utc()),
            at_s(0),
        )
        .unwrap();
    let at = reading_at(0, utc(2026, 7, 4, 16, 30, 5));
    assert_eq!(clocks.reading(&name("show"), at).unwrap().text, "300");
    assert_eq!(clocks.reading(&name("wall"), at).unwrap().text, "16:30:05");

    // Time order is shared across all clocks in the registry.
    clocks
        .apply(&name("show"), ClockCommand::Start, at_s(10))
        .unwrap();
    assert_eq!(
        clocks.apply(&name("stream-1"), ClockCommand::SessionStarted, at_s(9)),
        Err(ProductionClockError::TimeRegression {
            last: at_s(10),
            attempted: at_s(9)
        })
    );
    assert!(clocks.poll(at_s(9)).is_err());
}

#[test]
fn registry_poll_names_expired_timers() {
    let mut clocks = studio();
    clocks
        .insert(
            name("break"),
            ProductionClock::timer(
                TimerConfig::countdown(secs(30), OverrunPolicy::StopAtTarget),
                DurationFormat::MinutesSeconds,
            )
            .unwrap(),
        )
        .unwrap();
    clocks
        .apply(&name("show"), ClockCommand::Start, at_s(0))
        .unwrap();
    clocks
        .apply(&name("break"), ClockCommand::Start, at_s(0))
        .unwrap();
    assert_eq!(clocks.poll(at_s(29)).unwrap(), vec![]);
    assert_eq!(
        clocks.poll(at_s(400)).unwrap(),
        vec![
            (name("break"), TimerEvent::Expired { at: at_s(30) }),
            (name("show"), TimerEvent::Expired { at: at_s(300) }),
        ]
    );
    assert_eq!(clocks.poll(at_s(401)).unwrap(), vec![]);
}

#[test]
fn registry_capacity_and_duplicates() {
    let mut clocks = ProductionClocks::new();
    for index in 0..MAX_PRODUCTION_CLOCKS {
        clocks
            .insert(
                name(&format!("c{index}")),
                ProductionClock::media_remaining(DurationFormat::Seconds),
            )
            .unwrap();
    }
    assert_eq!(
        clocks.insert(
            name("extra"),
            ProductionClock::media_remaining(DurationFormat::Seconds)
        ),
        Err(ProductionClockError::CapacityExceeded)
    );
    assert_eq!(
        clocks.insert(
            name("c0"),
            ProductionClock::media_remaining(DurationFormat::Seconds)
        ),
        Err(ProductionClockError::DuplicateClock(name("c0")))
    );
    clocks.remove(&name("c0")).unwrap();
    assert_eq!(clocks.len(), MAX_PRODUCTION_CLOCKS - 1);
    assert!(clocks.remove(&name("c0")).is_err());
}

#[test]
fn stepping_the_wall_clock_moves_time_of_day_but_never_timers() {
    let domain = ClockDomainId::new(NonZeroU128::new(7).unwrap());
    let mut monotonic = ManualClock::new(domain, at_s(0));
    let mut wall = ManualWallClock::new(utc(2026, 3, 8, 6, 59, 0));
    let mut clocks = studio();
    clocks
        .apply(&name("show"), ClockCommand::Start, monotonic.now())
        .unwrap();

    monotonic
        .advance(ClockDuration::from_nanos(60 * 1_000_000_000))
        .unwrap();
    wall.set(utc(2026, 3, 8, 7, 0, 0));
    let first = TimeReading::sample(&monotonic, &wall);
    assert_eq!(clocks.reading(&name("show"), first).unwrap().text, "04:00");
    assert_eq!(
        clocks.reading(&name("wall"), first).unwrap().text,
        "03:00:00"
    );

    // An NTP step backward by an hour.
    wall.set(UtcInstant::from_unix_seconds(wall.utc_now().unix_seconds() - 3_600).unwrap());
    let stepped = TimeReading::sample(&monotonic, &wall);
    assert_eq!(
        clocks.reading(&name("show"), stepped).unwrap().text,
        "04:00"
    );
    assert_eq!(
        clocks.reading(&name("wall"), stepped).unwrap().text,
        "01:00:00"
    );
}

#[test]
fn reading_with_format_reformats_one_read_without_changing_the_clock() {
    let mut clocks = ProductionClocks::new();
    clocks
        .insert(
            name("down"),
            ProductionClock::timer(
                TimerConfig::countdown(secs(90), OverrunPolicy::Continue),
                DurationFormat::MinutesSeconds,
            )
            .unwrap(),
        )
        .unwrap();
    clocks
        .insert(
            name("tod"),
            ProductionClock::time_of_day(TimeZone::utc(), TimeOfDayFormat::Hours24),
        )
        .unwrap();
    clocks
        .apply(&name("down"), ClockCommand::Start, at_s(0))
        .unwrap();
    let at = TimeReading::new(at_ms(500), utc(2026, 3, 8, 13, 5, 9));

    let plain = clocks.reading(&name("down"), at).unwrap();
    let compact = clocks
        .reading_with_format(
            &name("down"),
            at,
            DisplayFormat::Duration(DurationFormat::Compact),
        )
        .unwrap();
    assert_eq!(plain.text, "01:30");
    // Same rounding (countdown ceil), same state and value; only text differs.
    assert_eq!(compact.text, "1:30");
    assert_eq!(compact.value, plain.value);
    assert_eq!(compact.state, plain.state);
    assert_eq!(
        clocks.get(&name("down")).unwrap().display_format(),
        DisplayFormat::Duration(DurationFormat::MinutesSeconds)
    );

    let twelve = clocks
        .reading_with_format(
            &name("tod"),
            at,
            DisplayFormat::TimeOfDay(TimeOfDayFormat::Hours12Minutes),
        )
        .unwrap();
    assert_eq!(twelve.text, "1:05 PM");

    assert_eq!(
        clocks.reading_with_format(
            &name("tod"),
            at,
            DisplayFormat::Duration(DurationFormat::Seconds)
        ),
        Err(ProductionClockError::FormatMismatch)
    );
    assert!(
        !clocks
            .get(&name("down"))
            .unwrap()
            .accepts_format(DisplayFormat::TimeOfDay(TimeOfDayFormat::Date))
    );
    assert_eq!(
        clocks.reading_with_format(
            &name("absent"),
            at,
            DisplayFormat::Duration(DurationFormat::Seconds)
        ),
        Err(ProductionClockError::UnknownClock(name("absent")))
    );
}
