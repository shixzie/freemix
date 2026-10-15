//! Backend-independent media-file transport: rational position, seek, pause,
//! trim, loop, rate, end actions, audio-track selection, and growing sources.

use fm_playback::{
    AudioPlan, AudioSpeedPolicy, AudioTrack, FrameIndex, MediaControlError, MediaEndAction,
    MediaEvent, MediaPlayback, MediaSourceConfig, MediaState, PlaybackRate, RationalSeconds,
    resample_linear,
};
use fm_types::FrameRate;

fn rate(numerator: u32, denominator: u32) -> FrameRate {
    FrameRate::new(numerator, denominator).unwrap()
}

fn track(stream_index: u32, sample_rate: u32) -> AudioTrack {
    AudioTrack {
        stream_index,
        sample_rate,
        channels: 2,
        language: None,
        title: None,
        default: false,
    }
}

fn transport(frames: u64, complete: bool) -> MediaPlayback {
    MediaPlayback::new(MediaSourceConfig {
        source_rate: rate(25, 1),
        output_rate: rate(25, 1),
        available_frames: frames,
        complete,
        audio_tracks: vec![track(1, 48_000), track(2, 44_100)],
        selected_audio: Some(1),
    })
    .unwrap()
}

fn frames(transport: &mut MediaPlayback, ticks: usize) -> Vec<u64> {
    (0..ticks)
        .map(|_| transport.tick().frame.map_or(u64::MAX, FrameIndex::get))
        .collect()
}

#[test]
fn stopped_transport_cues_frame_zero_and_play_pause_resume_are_frame_exact() {
    let mut transport = transport(100, true);
    let cued = transport.tick();
    assert_eq!(cued.frame, Some(FrameIndex::new(0)));
    assert_eq!(cued.state, MediaState::Stopped);
    assert!(cued.discontinuity);
    assert_eq!(
        cued.audio,
        AudioPlan::Silence {
            track: 1,
            output_samples: 1_920
        }
    );

    transport.play();
    assert_eq!(frames(&mut transport, 5), [0, 1, 2, 3, 4]);
    transport.pause();
    let paused = transport.tick();
    assert_eq!(paused.frame, Some(FrameIndex::new(5)));
    assert_eq!(paused.state, MediaState::Paused);
    let held = transport.tick();
    assert!(held.repeated);
    assert!(!held.discontinuity);
    transport.play();
    assert_eq!(frames(&mut transport, 3), [5, 6, 7]);
    assert_eq!(transport.position(), FrameIndex::new(8));
    assert_eq!(
        transport.position_time(),
        RationalSeconds::new(8, 25).unwrap()
    );
}

#[test]
fn seek_is_exact_flags_a_discontinuity_and_respects_the_playable_range() {
    let mut transport = transport(100, true);
    transport.play();
    frames(&mut transport, 3);
    transport.seek(FrameIndex::new(57)).unwrap();
    let tick = transport.tick();
    assert_eq!(tick.frame, Some(FrameIndex::new(57)));
    assert!(tick.discontinuity);
    assert_eq!(tick.dropped_frames, 0);
    assert_eq!(
        tick.events,
        [MediaEvent::Seeked {
            frame: FrameIndex::new(57)
        }]
    );
    assert_eq!(frames(&mut transport, 2), [58, 59]);

    transport
        .seek_time(RationalSeconds::new(3, 2).unwrap())
        .unwrap();
    assert_eq!(transport.position(), FrameIndex::new(37));
    assert_eq!(
        transport.seek(FrameIndex::new(100)),
        Err(MediaControlError::SeekOutOfRange {
            requested: FrameIndex::new(100),
            start: FrameIndex::new(0),
            end: FrameIndex::new(100),
        })
    );
    transport
        .set_trim(FrameIndex::new(10), Some(FrameIndex::new(20)))
        .unwrap();
    assert!(transport.seek(FrameIndex::new(9)).is_err());
    assert!(transport.seek(FrameIndex::new(20)).is_err());
    transport.seek(FrameIndex::new(19)).unwrap();
}

#[test]
fn trim_loops_seamlessly_and_counts_passes() {
    let mut transport = transport(100, true);
    transport
        .set_trim(FrameIndex::new(10), Some(FrameIndex::new(14)))
        .unwrap();
    assert_eq!(transport.position(), FrameIndex::new(10));
    transport.set_end_action(MediaEndAction::Loop);
    transport.play();
    let mut looped = Vec::new();
    let mut presented = Vec::new();
    for _ in 0..10 {
        let tick = transport.tick();
        presented.push(tick.frame.unwrap().get());
        looped.extend(tick.events.into_iter().filter_map(|event| match event {
            MediaEvent::Looped { count } => Some(count),
            _ => None,
        }));
        if presented.len() == 5 {
            assert!(tick.discontinuity, "first frame after a loop repositions");
        }
    }
    assert_eq!(presented, [10, 11, 12, 13, 10, 11, 12, 13, 10, 11]);
    assert_eq!(looped, [1, 2]);
    assert_eq!(transport.loop_count(), 2);

    // Without trim, looping covers the whole file.
    transport.clear_trim();
    transport.seek(FrameIndex::new(98)).unwrap();
    assert_eq!(frames(&mut transport, 4), [98, 99, 0, 1]);
}

#[test]
fn trim_validation_rejects_reversed_and_out_of_source_points() {
    let mut transport = transport(100, true);
    assert_eq!(
        transport.set_trim(FrameIndex::new(5), Some(FrameIndex::new(5))),
        Err(MediaControlError::InvalidTrim {
            in_frame: FrameIndex::new(5),
            out_frame: FrameIndex::new(5)
        })
    );
    assert!(matches!(
        transport.set_trim(FrameIndex::new(5), Some(FrameIndex::new(101))),
        Err(MediaControlError::TrimBeyondSource { .. })
    ));
    assert!(matches!(
        transport.set_trim(FrameIndex::new(100), None),
        Err(MediaControlError::TrimBeyondSource { .. })
    ));
    assert_eq!(transport.trim(), (FrameIndex::new(0), None));
}

#[test]
fn end_actions_hold_stop_and_emit_events() {
    let mut transport = transport(10, true);
    transport.set_end_trigger_lead(Some(3));
    transport.seek(FrameIndex::new(5)).unwrap();
    transport.play();
    let mut events = Vec::new();
    let mut presented = Vec::new();
    for _ in 0..7 {
        let tick = transport.tick();
        presented.push(tick.frame.unwrap().get());
        events.extend(tick.events);
    }
    assert_eq!(presented, [5, 6, 7, 8, 9, 9, 9]);
    assert_eq!(transport.state(), MediaState::Ended);
    assert!(events.contains(&MediaEvent::EndTrigger {
        remaining_frames: 3
    }));
    assert!(events.contains(&MediaEvent::Ended {
        action: MediaEndAction::HoldLastFrame
    }));
    // Play after an end restarts from trim-in.
    transport.play();
    assert_eq!(frames(&mut transport, 2), [0, 1]);

    transport.set_end_action(MediaEndAction::Stop);
    transport.seek(FrameIndex::new(8)).unwrap();
    let ticks: Vec<_> = (0..3).map(|_| transport.tick()).collect();
    assert_eq!(ticks[1].state, MediaState::Stopped);
    assert_eq!(ticks[2].frame, Some(FrameIndex::new(0)));
    assert!(ticks[2].discontinuity);
    assert_eq!(transport.state(), MediaState::Stopped);
}

#[test]
fn fast_rates_drop_frames_and_slow_rates_duplicate_them() {
    let mut transport = transport(100, true);
    transport.play();
    transport
        .set_rate(PlaybackRate::new(2, 1).unwrap())
        .unwrap();
    let ticks: Vec<_> = (0..4).map(|_| transport.tick()).collect();
    let presented: Vec<_> = ticks.iter().map(|tick| tick.frame.unwrap().get()).collect();
    assert_eq!(presented, [0, 2, 4, 6]);
    assert_eq!(ticks[1].dropped_frames, 1);

    transport.seek(FrameIndex::new(0)).unwrap();
    transport
        .set_rate(PlaybackRate::from_basis_points(2_500).unwrap())
        .unwrap();
    let ticks: Vec<_> = (0..9).map(|_| transport.tick()).collect();
    let presented: Vec<_> = ticks.iter().map(|tick| tick.frame.unwrap().get()).collect();
    assert_eq!(presented, [0, 0, 0, 0, 1, 1, 1, 1, 2]);
    assert!(ticks[1].repeated && ticks[2].repeated && !ticks[4].repeated);

    // 1.5x: steps of 3/2 alternate one and two source frames.
    transport.seek(FrameIndex::new(0)).unwrap();
    transport
        .set_rate(PlaybackRate::new(3, 2).unwrap())
        .unwrap();
    assert_eq!(frames(&mut transport, 5), [0, 1, 3, 4, 6]);

    // A rate change never crosses a frame boundary: the playhead at 7.5 is
    // requantized down onto the 1/3 grid (22/3), not up to frame 8.
    transport
        .set_rate(PlaybackRate::new(1, 3).unwrap())
        .unwrap();
    assert_eq!(transport.position(), FrameIndex::new(7));
    assert_eq!(frames(&mut transport, 4), [7, 7, 8, 8]);

    assert!(PlaybackRate::new(1, 5).is_err());
    assert!(PlaybackRate::new(41, 10).is_err());
    assert!(PlaybackRate::new(0, 1).is_err());
    assert_eq!(
        PlaybackRate::from_basis_points(40_000).unwrap().numerator(),
        4
    );
}

#[test]
fn differing_source_and_output_rates_follow_the_rational_step() {
    let mut transport = MediaPlayback::new(MediaSourceConfig {
        source_rate: rate(25, 1),
        output_rate: rate(50, 1),
        available_frames: 100,
        complete: true,
        audio_tracks: Vec::new(),
        selected_audio: None,
    })
    .unwrap();
    transport.play();
    assert_eq!(frames(&mut transport, 6), [0, 0, 1, 1, 2, 2]);
    assert_eq!(transport.frames_per_tick(), (1, 2));

    let mut transport = MediaPlayback::new(MediaSourceConfig {
        source_rate: rate(30_000, 1_001),
        output_rate: rate(25, 1),
        available_frames: 2_000,
        complete: true,
        audio_tracks: Vec::new(),
        selected_audio: None,
    })
    .unwrap();
    transport.play();
    // 29.97 on a 25 grid: 30000/25025 source frames per tick; exactly 1198
    // whole source frames are consumed after 1000 ticks — no drift.
    for _ in 0..1_000 {
        transport.tick();
    }
    assert_eq!(
        transport.position(),
        FrameIndex::new(1_000 * 30_000 / 25_025)
    );
}

#[test]
fn normal_rate_audio_is_contiguous_and_resyncs_after_seek() {
    let mut transport = transport(100, true);
    transport.play();
    let mut next = 0;
    for _ in 0..30 {
        let AudioPlan::Source {
            track,
            start_sample,
            end_sample,
            output_samples,
            resample,
        } = transport.tick().audio
        else {
            panic!("playing audio must come from the source");
        };
        assert_eq!(track, 1);
        assert!(!resample);
        assert_eq!(start_sample, next);
        assert_eq!(end_sample - start_sample, u64::from(output_samples));
        assert_eq!(output_samples, 1_920);
        next = end_sample;
    }
    transport.seek(FrameIndex::new(50)).unwrap();
    let AudioPlan::Source { start_sample, .. } = transport.tick().audio else {
        panic!("source audio expected");
    };
    assert_eq!(start_sample, 50 * 1_920);
}

#[test]
fn audio_track_switch_mid_play_continues_at_the_exact_sample() {
    let mut transport = transport(100, true);
    transport.play();
    frames(&mut transport, 10);
    transport.select_audio_track(Some(2)).unwrap();
    let tick = transport.tick();
    assert_eq!(
        tick.events,
        [MediaEvent::AudioTrackChanged {
            from: Some(1),
            to: Some(2)
        }]
    );
    let AudioPlan::Source {
        track,
        start_sample,
        output_samples,
        ..
    } = tick.audio
    else {
        panic!("source audio expected");
    };
    assert_eq!(track, 2);
    // Frame 10 at 25 fps = 0.4 s = sample 17_640 at 44.1 kHz.
    assert_eq!(start_sample, 17_640);
    assert_eq!(output_samples, 1_764);
    assert_eq!(
        transport.select_audio_track(Some(9)),
        Err(MediaControlError::UnknownAudioTrack(9))
    );
    transport.select_audio_track(None).unwrap();
    assert_eq!(transport.tick().audio, AudioPlan::None);
}

#[test]
fn non_normal_rates_mute_or_resample_the_exact_covered_span() {
    let mut transport = transport(100, true);
    transport.play();
    transport
        .set_rate(PlaybackRate::new(2, 1).unwrap())
        .unwrap();
    assert_eq!(
        transport.tick().audio,
        AudioPlan::Silence {
            track: 1,
            output_samples: 1_920
        }
    );
    transport.set_audio_policy(AudioSpeedPolicy::Resample);
    assert_eq!(
        transport.tick().audio,
        AudioPlan::Source {
            track: 1,
            start_sample: 2 * 1_920,
            end_sample: 4 * 1_920,
            output_samples: 1_920,
            resample: true,
        }
    );
    transport
        .set_rate(PlaybackRate::new(1, 2).unwrap())
        .unwrap();
    assert_eq!(
        transport.tick().audio,
        AudioPlan::Source {
            track: 1,
            start_sample: 4 * 1_920,
            end_sample: 4 * 1_920 + 960,
            output_samples: 1_920,
            resample: true,
        }
    );
}

#[test]
fn linear_resampler_is_deterministic() {
    assert_eq!(resample_linear(&[0.0, 1.0], 4), [0.0, 0.5, 1.0, 1.0]);
    assert_eq!(resample_linear(&[0.0, 1.0, 2.0, 3.0], 2), [0.0, 2.0]);
    assert_eq!(resample_linear(&[], 3), [0.0, 0.0, 0.0]);
    assert_eq!(resample_linear(&[0.25, 0.5], 2), [0.25, 0.5]);
}

#[test]
fn growing_source_waits_at_the_live_edge_and_resumes_without_skipping() {
    let mut transport = transport(0, false);
    transport.set_live_edge_offset(2);
    transport.set_end_trigger_lead(Some(1));
    transport.play();
    let starved = transport.tick();
    assert_eq!(starved.frame, None);
    assert_eq!(starved.state, MediaState::Waiting);

    transport.update_source(5, false).unwrap();
    let mut presented = Vec::new();
    let mut events = Vec::new();
    for _ in 0..5 {
        let tick = transport.tick();
        presented.push(tick.frame.unwrap().get());
        events.extend(tick.events);
    }
    // Live edge = 5 - 2 = 3 frames playable; the last one is held.
    assert_eq!(presented, [0, 1, 2, 2, 2]);
    assert_eq!(transport.state(), MediaState::Waiting);
    assert!(events.contains(&MediaEvent::LiveEdgeReached {
        frame: FrameIndex::new(3)
    }));
    assert!(
        !events.iter().any(|event| matches!(
            event,
            MediaEvent::EndTrigger { .. } | MediaEvent::Ended { .. }
        )),
        "a live edge is not an end"
    );

    transport.update_source(8, false).unwrap();
    let tick = transport.tick();
    assert_eq!(tick.frame, Some(FrameIndex::new(3)));
    assert!(tick.events.contains(&MediaEvent::LiveEdgeResumed {
        frame: FrameIndex::new(3)
    }));
    assert_eq!(
        transport.update_source(7, false),
        Err(MediaControlError::SourceTruncated {
            previous_frames: 8,
            frames: 7
        })
    );

    transport.update_source(9, true).unwrap();
    let mut presented = Vec::new();
    let mut events = Vec::new();
    for _ in 0..7 {
        let tick = transport.tick();
        presented.push(tick.frame.unwrap().get());
        events.extend(tick.events);
    }
    assert_eq!(presented, [4, 5, 6, 7, 8, 8, 8]);
    assert!(events.contains(&MediaEvent::SourceCompleted { frames: 9 }));
    assert!(events.contains(&MediaEvent::Ended {
        action: MediaEndAction::HoldLastFrame
    }));
    assert_eq!(
        transport.update_source(10, true),
        Err(MediaControlError::CompletedSourceChanged {
            frames: 9,
            reported: 10
        })
    );
}

#[test]
fn trim_out_inside_available_data_is_a_final_end_even_while_growing() {
    let mut transport = transport(50, false);
    transport.set_live_edge_offset(1);
    transport
        .set_trim(FrameIndex::new(0), Some(FrameIndex::new(3)))
        .unwrap();
    transport.set_end_action(MediaEndAction::Loop);
    transport.play();
    assert_eq!(frames(&mut transport, 5), [0, 1, 2, 0, 1]);
}
