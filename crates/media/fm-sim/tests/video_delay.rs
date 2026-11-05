//! IN-022: the simulated render path applies per-input video delays.

use core::num::NonZeroU128;

use fm_sim::{Rgba8, SimulatedPipeline, SimulatedSource, SourcePattern};
use fm_switcher::ProgramFrame;
use fm_types::InputId;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn program(primary: InputId) -> ProgramFrame {
    ProgramFrame {
        primary,
        secondary: None,
        transition_kind: None,
        mix_numerator: 0,
        mix_denominator: 1,
        mix_start_numerator: 0,
        mix_end_numerator: 0,
    }
}

fn pipeline() -> SimulatedPipeline {
    let mut pipeline = SimulatedPipeline::new(4, 2).unwrap();
    pipeline
        .register(SimulatedSource::new(input(1), SourcePattern::Bars))
        .unwrap();
    pipeline
        .register(SimulatedSource::new(
            input(2),
            SourcePattern::Solid(Rgba8::new(255, 0, 0, 255)),
        ))
        .unwrap();
    pipeline
}

const BLACK: Rgba8 = Rgba8::new(0, 0, 0, 255);

#[test]
fn a_delayed_source_holds_black_until_the_delay_fills() {
    let mut pipeline = pipeline();
    pipeline.set_video_delay(input(1), 2);
    assert_eq!(pipeline.video_delay(input(1)), 2);
    assert_eq!(pipeline.presented_frame(input(1), 0), None);
    assert_eq!(pipeline.presented_frame(input(1), 1), None);
    assert_eq!(pipeline.presented_frame(input(1), 2), Some(0));
    assert_eq!(pipeline.presented_frame(input(1), 3), Some(1));

    for frame in [0, 1] {
        assert_eq!(
            pipeline.render_source(input(1), frame).unwrap().pixel(0, 0),
            Some(BLACK)
        );
    }
    // From the delay onward the bars picture is shown; it is static, so the
    // delayed frame and the current frame have the same pixels.
    let bars = pipeline.render_source(input(1), 2).unwrap();
    assert_eq!(
        bars.pixel(0, 0),
        pipeline.render_source(input(1), 3).unwrap().pixel(0, 0)
    );
    assert_ne!(bars.pixel(0, 0), Some(BLACK));

    // A program render at the same frame holds black too.
    assert_eq!(
        pipeline.render(1, program(input(1))).unwrap().pixel(0, 0),
        Some(BLACK)
    );
    assert_eq!(
        pipeline.render(2, program(input(1))).unwrap().pixel(0, 0),
        Some(bars.pixel(0, 0).unwrap())
    );
}

#[test]
fn each_input_applies_its_own_delay() {
    let mut pipeline = pipeline();
    pipeline.set_video_delay(input(1), 2);
    // Input 2 is undelayed and shows red immediately.
    assert_eq!(
        pipeline.render(0, program(input(2))).unwrap().pixel(0, 0),
        Some(Rgba8::new(255, 0, 0, 255))
    );
    assert_eq!(
        pipeline.render(1, program(input(1))).unwrap().pixel(0, 0),
        Some(BLACK)
    );
    assert_eq!(pipeline.presented_frame(input(2), 0), Some(0));
}

#[test]
fn delays_are_clamped_and_zero_clears_them() {
    let mut pipeline = pipeline();
    pipeline.set_video_delay(input(1), u32::MAX);
    assert_eq!(
        pipeline.video_delay(input(1)),
        SimulatedPipeline::MAX_VIDEO_DELAY_FRAMES
    );
    pipeline.set_video_delay(input(1), 0);
    assert_eq!(pipeline.video_delay(input(1)), 0);
    assert_eq!(pipeline.presented_frame(input(1), 0), Some(0));
    assert_ne!(
        pipeline.render_source(input(1), 0).unwrap().pixel(0, 0),
        Some(BLACK)
    );
}
