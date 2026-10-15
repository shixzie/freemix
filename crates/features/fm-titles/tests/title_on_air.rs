//! `TitleAir`: In/Hold/Out against output frames at any output rate.

use fm_titles::fm_animation::{ClipPosition, ClipState, Phase, Timebase};
use fm_titles::{TitleAir, TitleAirFrame, TitleAirPhase, TitleAirTiming};
use fm_types::FrameRate;

fn rate(numerator: u32, denominator: u32) -> FrameRate {
    FrameRate::new(numerator, denominator).unwrap()
}

fn timing(in_frames: u32, out_frames: u32) -> TitleAirTiming {
    TitleAirTiming {
        timebase: Timebase::new(rate(25, 1)),
        in_frames,
        out_frames,
    }
}

#[test]
fn phases_last_their_exact_output_length_and_sample_rationally() {
    // 25p clip on a 50p output: 8 clip frames take 16 output frames.
    let air = TitleAir::take(timing(8, 4), rate(50, 1), 100);
    assert_eq!(air.phase_frames(), 16);
    assert_eq!(
        air.at(103),
        Some(TitleAirFrame {
            phase: TitleAirPhase::In,
            phase_frame: 3,
            phase_frames: 16,
            sample: ClipState::In(ClipPosition::new(3, 2)),
        })
    );
    assert_eq!(air.at(115).unwrap().phase, TitleAirPhase::In);
    assert_eq!(air.at(116).unwrap().sample, ClipState::Hold);
    assert_eq!(air.settles_at(100), 116);

    let leaving = air.off_air(200);
    assert!(leaving.is_leaving());
    assert_eq!(leaving.phase_frames(), 8);
    assert_eq!(
        leaving.at(207).unwrap().sample,
        ClipState::Out(ClipPosition::new(7, 2))
    );
    assert_eq!(leaving.at(208), None);
}

#[test]
fn off_air_during_in_starts_out_and_retake_during_out_restarts_in() {
    let air = TitleAir::take(timing(10, 6), rate(25, 1), 0);
    let leaving = air.off_air(4);
    assert_eq!(
        leaving.at(4).unwrap().sample,
        ClipState::Out(ClipPosition::frame(0))
    );
    // Off air again while leaving changes nothing.
    assert_eq!(leaving.off_air(6), leaving);
    let back = leaving.retake(7);
    assert_eq!(back.start_frame(), 7);
    assert_eq!(
        back.at(7).unwrap().sample,
        ClipState::In(ClipPosition::frame(0))
    );
    // Re-taking a title already in or holding changes nothing.
    assert_eq!(back.retake(9), back);
}

#[test]
fn zero_length_phases_hold_and_leave_on_their_start_frame() {
    let air = TitleAir::take(TitleAirTiming::default(), rate(60, 1), 5);
    assert_eq!(air.at(5).unwrap().phase, TitleAirPhase::Hold);
    assert_eq!(air.off_air(9).at(9), None);
}

#[test]
fn a_skipped_frame_lands_on_its_exact_pose() {
    // A renderer that only sees every seventh frame still gets each pose
    // exactly, because the pose is a function of the frame alone.
    let timing = timing(30, 0);
    let output = rate(30_000, 1_001);
    let air = TitleAir::take(timing, output, 1_000);
    for frame in (1_000..1_040).step_by(7) {
        let expected = if frame - 1_000 < timing.output_frames(Phase::In, output) {
            ClipState::In(timing.timebase.position_at(frame - 1_000, output))
        } else {
            ClipState::Hold
        };
        assert_eq!(air.at(frame).unwrap().sample, expected, "frame {frame}");
    }
}
