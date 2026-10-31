//! SW-013: a project's named production clocks become a live engine
//! registry, controlled on the engine's monotonic clock.

use core::num::NonZeroU128;

use fm_clock::production::{
    ClockCommand, ClockName, ReadingState, TimeReading, UtcInstant,
};
use fm_clock::ClockDomainId;
use fm_engine::{Engine, EngineMediaSource, ShowState};
use fm_types::{
    FrameRate, InputId, ProjectClock, ProjectClockDirection, ProjectClockKind,
    ProjectClockOverrun, ProjectDurationFormat, ProjectTimeFormat,
};
use fm_model::MediaPlaybackSettings;

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(nonzero(value))
}

fn defs() -> Vec<ProjectClock> {
    vec![
        ProjectClock {
            name: "Countdown".into(),
            kind: ProjectClockKind::Timer {
                direction: ProjectClockDirection::CountDown,
                preset_nanos: 60_000_000_000,
                limit_nanos: None,
                overrun: ProjectClockOverrun::StopAtTarget,
                format: ProjectDurationFormat::MinutesSeconds,
            },
        },
        ProjectClock {
            name: "Local".into(),
            kind: ProjectClockKind::TimeOfDay {
                zone: "UTC0".into(),
                format: ProjectTimeFormat::Hours24,
            },
        },
        ProjectClock {
            name: "Media".into(),
            kind: ProjectClockKind::MediaRemaining {
                input: Some(input(3)),
                format: ProjectDurationFormat::MinutesSeconds,
            },
        },
    ]
}

fn engine() -> Engine {
    let mut show = ShowState::new(
        "production clocks",
        vec![
            (input(1), "Program".into()),
            (input(2), "Preview".into()),
            (input(3), "Clip".into()),
        ],
        input(1),
        input(2),
    )
    .unwrap();
    show.set_media_input(input(3), MediaPlaybackSettings::default())
        .unwrap();
    show.set_production_clocks(defs()).unwrap();
    let mut engine = Engine::new(
        show,
        FrameRate::new(30, 1).unwrap(),
        ClockDomainId::new(nonzero(100)),
    );
    engine
        .set_media_source(
            input(3),
            Some(EngineMediaSource::synthetic(
                &MediaPlaybackSettings::default(),
                FrameRate::new(30, 1).unwrap(),
            )),
        )
        .unwrap();
    engine
}

fn name(value: &str) -> ClockName {
    ClockName::new(value).unwrap()
}

fn reading_at(engine: &Engine, clock: &str) -> fm_clock::production::ClockReading {
    engine
        .production_clock_registry()
        .reading(
            &name(clock),
            TimeReading::new(engine.clock_time(), UtcInstant::UNIX_EPOCH),
        )
        .unwrap()
}

#[test]
fn timers_run_on_the_engine_clock_and_pause() {
    let mut engine = engine();
    assert_eq!(engine.production_clock_registry().len(), 3);
    let idle = reading_at(&engine, "Countdown");
    assert_eq!(idle.state, ReadingState::Idle);
    assert_eq!(idle.text, "01:00");

    engine
        .control_production_clock("Countdown", ClockCommand::Start)
        .unwrap();
    // Tick 1 realizes frame 0 at its deadline, so 31 ticks span 30 full
    // frames, exactly one second at 30/1.
    for _ in 0..31 {
        engine.tick().unwrap();
    }
    let running = reading_at(&engine, "Countdown");
    assert_eq!(running.state, ReadingState::Running);
    assert_eq!(running.value.unwrap().as_millis(), 59_000);

    engine
        .control_production_clock("Countdown", ClockCommand::Pause)
        .unwrap();
    let paused = reading_at(&engine, "Countdown");
    assert_eq!(paused.state, ReadingState::Paused);
    let paused_value = paused.value.unwrap();
    for _ in 0..30 {
        engine.tick().unwrap();
    }
    let after = reading_at(&engine, "Countdown");
    assert_eq!(after.value.unwrap(), paused_value);

    engine
        .control_production_clock(
            "Countdown",
            ClockCommand::SetValue(fm_clock::production::ClockValue::from_nanos(5_000_000_000).unwrap()),
        )
        .unwrap();
    assert_eq!(reading_at(&engine, "Countdown").text, "00:05");
}

#[test]
fn a_media_clock_follows_its_input() {
    let mut engine = engine();
    let before = reading_at(&engine, "Media");
    let _ = engine.tick().unwrap();
    let after = reading_at(&engine, "Media");
    assert_eq!(after.state, ReadingState::Live, "{after:?}");
    assert!(after.value.is_some(), "{after:?}");
    assert_ne!(before.state, ReadingState::Live);
}

#[test]
fn time_of_day_uses_the_reading_instant() {
    let engine = engine();
    let reading = engine
        .production_clock_registry()
        .reading(
            &name("Local"),
            TimeReading::new(engine.clock_time(), UtcInstant::new(12 * 3_600, 0).unwrap()),
        )
        .unwrap();
    assert_eq!(reading.state, ReadingState::Live);
    assert_eq!(reading.text, "12:00:00");
}

#[test]
fn unknown_clocks_and_kind_mismatches_are_refused() {
    let mut engine = engine();
    assert!(
        engine
            .control_production_clock("Missing", ClockCommand::Start)
            .is_err()
    );
    assert!(
        engine
            .control_production_clock("Local", ClockCommand::Start)
            .is_err()
    );
    assert_eq!(reading_at(&engine, "Countdown").state, ReadingState::Idle);
}
