use fm_automation::{
    AdvanceCause, EntryDuration, LatePolicy, LoopMode, MAX_PLAYLIST_ENTRIES, MAX_PLAYLIST_ENTRY_MS,
    MAX_PLAYLIST_TIME_MS, Playlist, PlaylistCommand, PlaylistConfig, PlaylistEntry,
    PlaylistEntryId, PlaylistError, PlaylistEvent, PlaylistEventKind, PlaylistSnapshot,
    PlaylistState, PlaylistTransition, PlaylistTransitionKind, SnapshotError, StartAt,
    decode_playlist_snapshot, encode_playlist_snapshot,
};
use fm_types::InputId;
use std::num::NonZeroU128;

fn id(value: u64) -> PlaylistEntryId {
    PlaylistEntryId::new(value)
}

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn fixed(entry: u64, ms: u64) -> PlaylistEntry {
    PlaylistEntry::new(
        id(entry),
        input(u128::from(entry) + 100),
        EntryDuration::Fixed { ms },
    )
}

fn media(entry: u64, length_ms: Option<u64>) -> PlaylistEntry {
    PlaylistEntry::new(
        id(entry),
        input(u128::from(entry) + 100),
        EntryDuration::Media { length_ms },
    )
}

fn config(loop_mode: LoopMode, late_policy: LatePolicy) -> PlaylistConfig {
    PlaylistConfig {
        loop_mode,
        late_policy,
    }
}

/// A (5 s, cut), B (3 s, 500 ms fade), C (2 s, cut).
fn abc(loop_mode: LoopMode) -> Playlist {
    Playlist::new(
        vec![
            fixed(1, 5_000),
            fixed(2, 3_000)
                .with_transition(PlaylistTransition::new(PlaylistTransitionKind::Fade, 500)),
            fixed(3, 2_000),
        ],
        config(loop_mode, LatePolicy::Reject),
    )
    .unwrap()
}

fn start_now() -> PlaylistCommand {
    PlaylistCommand::Start {
        at: StartAt::Now,
        from: None,
    }
}

/// `(at_ms, entry, offset_ms, cause)` for every `EntryOn`.
fn entry_ons(events: &[PlaylistEvent]) -> Vec<(u64, u64, u64, AdvanceCause)> {
    events
        .iter()
        .filter_map(|event| match event.kind {
            PlaylistEventKind::EntryOn {
                entry,
                offset_ms,
                cause,
                ..
            } => Some((event.at_ms, entry.get(), offset_ms, cause)),
            _ => None,
        })
        .collect()
}

fn skipped(events: &[PlaylistEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|event| match event.kind {
            PlaylistEventKind::Skipped { entry, .. } => Some(entry.get()),
            _ => None,
        })
        .collect()
}

fn on_air_id(playlist: &Playlist) -> Option<u64> {
    playlist.on_air().map(|entry| entry.id.get())
}

#[test]
fn empty_playlist_is_editable_but_cannot_start() {
    let mut playlist = Playlist::new(Vec::new(), PlaylistConfig::default()).unwrap();
    assert_eq!(playlist.apply(10, start_now()), Err(PlaylistError::Empty));
    assert_eq!(playlist.state(), PlaylistState::Stopped);
    // A failed command does not consume the time either.
    assert_eq!(playlist.observed_ms(), 0);
    assert_eq!(playlist.poll(20), Ok(Vec::new()));
    assert_eq!(
        playlist.apply(20, PlaylistCommand::Next),
        Err(PlaylistError::NotOnAir)
    );
    assert_eq!(
        playlist.apply(20, PlaylistCommand::Pause),
        Err(PlaylistError::NotOnAir)
    );
    assert_eq!(playlist.apply(20, PlaylistCommand::Stop), Ok(Vec::new()));
}

#[test]
fn all_disabled_entries_cannot_start_and_disabled_start_entry_is_refused() {
    let mut playlist = Playlist::new(
        vec![
            fixed(1, 1_000).with_enabled(false),
            fixed(2, 1_000).with_enabled(false),
        ],
        PlaylistConfig::default(),
    )
    .unwrap();
    assert_eq!(
        playlist.apply(0, start_now()),
        Err(PlaylistError::NoEnabledEntries)
    );
    assert_eq!(
        playlist.apply(
            0,
            PlaylistCommand::Start {
                at: StartAt::Now,
                from: Some(id(2)),
            }
        ),
        Err(PlaylistError::EntryDisabled(id(2)))
    );
    assert_eq!(
        playlist.apply(
            0,
            PlaylistCommand::Start {
                at: StartAt::Now,
                from: Some(id(9)),
            }
        ),
        Err(PlaylistError::UnknownEntry(id(9)))
    );
    assert_eq!(playlist.state(), PlaylistState::Stopped);
}

#[test]
fn disabling_everything_mid_air_finishes_at_the_current_boundary() {
    let mut playlist = abc(LoopMode::All);
    playlist.apply(0, start_now()).unwrap();
    for entry in [1, 2, 3] {
        playlist
            .apply(
                100,
                PlaylistCommand::SetEnabled {
                    entry: id(entry),
                    enabled: false,
                },
            )
            .unwrap();
    }
    // The on-air entry keeps running even though it is now disabled.
    assert_eq!(on_air_id(&playlist), Some(1));
    assert_eq!(playlist.poll(4_999), Ok(Vec::new()));
    let events = playlist.poll(5_000).unwrap();
    assert_eq!(
        events,
        vec![PlaylistEvent {
            at_ms: 5_000,
            kind: PlaylistEventKind::Finished { last: Some(id(1)) },
        }]
    );
    assert_eq!(
        playlist.state(),
        PlaylistState::Finished { last: Some(id(1)) }
    );
}

#[test]
fn entries_advance_on_exact_boundaries_with_their_own_transitions() {
    let mut playlist = abc(LoopMode::Off);
    let events = playlist.apply(1_000, start_now()).unwrap();
    assert_eq!(
        events[0],
        PlaylistEvent {
            at_ms: 1_000,
            kind: PlaylistEventKind::Armed { start_at_ms: 1_000 },
        }
    );
    assert_eq!(
        events[1],
        PlaylistEvent {
            at_ms: 1_000,
            kind: PlaylistEventKind::EntryOn {
                entry: id(1),
                input: input(101),
                transition: PlaylistTransition::CUT,
                take: 1,
                cycle: 0,
                offset_ms: 0,
                cause: AdvanceCause::Start,
            },
        }
    );
    assert_eq!(playlist.poll(5_999), Ok(Vec::new()));
    let events = playlist.poll(6_000).unwrap();
    assert_eq!(
        events,
        vec![PlaylistEvent {
            at_ms: 6_000,
            kind: PlaylistEventKind::EntryOn {
                entry: id(2),
                input: input(102),
                transition: PlaylistTransition::new(PlaylistTransitionKind::Fade, 500),
                take: 2,
                cycle: 0,
                offset_ms: 0,
                cause: AdvanceCause::Duration,
            },
        }]
    );
    assert_eq!(
        entry_ons(&playlist.poll(9_000).unwrap()),
        vec![(9_000, 3, 0, AdvanceCause::Duration)]
    );
    assert_eq!(
        playlist.poll(11_000).unwrap(),
        vec![PlaylistEvent {
            at_ms: 11_000,
            kind: PlaylistEventKind::Finished { last: Some(id(3)) },
        }]
    );
    assert_eq!(playlist.poll(50_000), Ok(Vec::new()));
}

#[test]
fn final_state_is_independent_of_poll_cadence() {
    let mut fine = abc(LoopMode::All);
    let mut coarse = abc(LoopMode::All);
    fine.apply(0, start_now()).unwrap();
    coarse.apply(0, start_now()).unwrap();
    let mut fine_ons = Vec::new();
    for now in 1..=23_500 {
        fine_ons.extend(entry_ons(&fine.poll(now).unwrap()));
    }
    let coarse_events = coarse.poll(23_500).unwrap();
    assert_eq!(fine.state(), coarse.state());
    assert_eq!(fine.snapshot(), coarse.snapshot());
    // Fine polling sees every boundary on time.
    assert_eq!(
        fine_ons
            .iter()
            .map(|(at, entry, offset, _)| (*at, *entry, *offset))
            .collect::<Vec<_>>(),
        vec![
            (5_000, 2, 0),
            (8_000, 3, 0),
            (10_000, 1, 0),
            (15_000, 2, 0),
            (18_000, 3, 0),
            (20_000, 1, 0),
        ]
    );
    // One late poll collapses the gap: only the on-air entry is switched to,
    // at its offset; partial cycles are reported as skipped and the whole
    // cycle in between is collapsed into the loop event.
    assert_eq!(skipped(&coarse_events), vec![2, 3]);
    assert!(coarse_events.contains(&PlaylistEvent {
        at_ms: 20_000,
        kind: PlaylistEventKind::Looped {
            cycle: 2,
            skipped_cycles: 1,
        },
    }));
    assert_eq!(
        entry_ons(&coarse_events),
        vec![(20_000, 1, 3_500, AdvanceCause::Duration)]
    );
}

#[test]
fn relative_start_waits_for_its_delay() {
    let mut playlist = abc(LoopMode::Off);
    let events = playlist
        .apply(
            2_000,
            PlaylistCommand::Start {
                at: StartAt::After { delay_ms: 1_500 },
                from: Some(id(2)),
            },
        )
        .unwrap();
    assert_eq!(
        events,
        vec![PlaylistEvent {
            at_ms: 2_000,
            kind: PlaylistEventKind::Armed { start_at_ms: 3_500 },
        }]
    );
    assert_eq!(
        playlist.state(),
        PlaylistState::Armed {
            start_at_ms: 3_500,
            from: Some(id(2)),
        }
    );
    assert_eq!(playlist.poll(3_499), Ok(Vec::new()));
    assert_eq!(
        entry_ons(&playlist.poll(3_500).unwrap()),
        vec![(3_500, 2, 0, AdvanceCause::Start)]
    );
    assert_eq!(
        playlist.apply(3_600, start_now()),
        Err(PlaylistError::AlreadyActive)
    );
}

#[test]
fn absolute_start_fires_at_wall_time_and_can_be_cancelled() {
    let mut playlist = abc(LoopMode::Off);
    playlist
        .apply(
            0,
            PlaylistCommand::Start {
                at: StartAt::At { wall_ms: 60_000 },
                from: None,
            },
        )
        .unwrap();
    assert_eq!(
        playlist.apply(10, PlaylistCommand::Pause),
        Err(PlaylistError::NotOnAir)
    );
    assert_eq!(playlist.poll(59_999), Ok(Vec::new()));
    let events = playlist.poll(60_250).unwrap();
    assert_eq!(
        entry_ons(&events),
        vec![(60_000, 1, 250, AdvanceCause::Start)]
    );

    let mut cancelled = abc(LoopMode::Off);
    cancelled
        .apply(
            0,
            PlaylistCommand::Start {
                at: StartAt::At { wall_ms: 60_000 },
                from: None,
            },
        )
        .unwrap();
    assert_eq!(
        cancelled.apply(30_000, PlaylistCommand::Stop).unwrap(),
        vec![PlaylistEvent {
            at_ms: 30_000,
            kind: PlaylistEventKind::Stopped,
        }]
    );
    assert_eq!(cancelled.poll(70_000), Ok(Vec::new()));
    assert_eq!(cancelled.state(), PlaylistState::Stopped);
}

#[test]
fn schedule_in_the_past_follows_the_late_policy() {
    let late_start = PlaylistCommand::Start {
        at: StartAt::At { wall_ms: 1_000 },
        from: None,
    };

    let mut reject = abc(LoopMode::Off);
    reject.poll(7_000).unwrap();
    assert_eq!(
        reject.apply(7_000, late_start),
        Err(PlaylistError::ScheduleInPast {
            start_at_ms: 1_000,
            now_ms: 7_000,
        })
    );
    assert_eq!(reject.state(), PlaylistState::Stopped);

    let mut start_now = abc(LoopMode::Off);
    start_now
        .apply(0, PlaylistCommand::SetLatePolicy(LatePolicy::StartNow))
        .unwrap();
    let events = start_now.apply(7_000, late_start).unwrap();
    assert_eq!(entry_ons(&events), vec![(7_000, 1, 0, AdvanceCause::Start)]);

    // Joining in progress: A ran 1000..6000, so B is on air 1 s in.
    let mut join = abc(LoopMode::Off);
    join.apply(
        0,
        PlaylistCommand::SetLatePolicy(LatePolicy::JoinInProgress),
    )
    .unwrap();
    let events = join.apply(7_000, late_start).unwrap();
    assert_eq!(skipped(&events), vec![1]);
    assert_eq!(
        entry_ons(&events),
        vec![(6_000, 2, 1_000, AdvanceCause::Duration)]
    );
    assert_eq!(
        join.state(),
        PlaylistState::Playing {
            entry: id(2),
            take: 2,
            cycle: 0,
            started_at_ms: 6_000,
        }
    );

    // Joining a schedule that has entirely elapsed finishes immediately.
    let mut elapsed = abc(LoopMode::Off);
    elapsed
        .apply(
            0,
            PlaylistCommand::SetLatePolicy(LatePolicy::JoinInProgress),
        )
        .unwrap();
    let events = elapsed.apply(60_000, late_start).unwrap();
    assert_eq!(skipped(&events), vec![1, 2, 3]);
    assert_eq!(
        events.last().unwrap(),
        &PlaylistEvent {
            at_ms: 11_000,
            kind: PlaylistEventKind::Finished { last: Some(id(3)) },
        }
    );
}

#[test]
fn pausing_across_an_entry_boundary_pauses_the_successor() {
    let mut playlist = abc(LoopMode::Off);
    playlist.apply(0, start_now()).unwrap();
    // A ends at 5000, but nobody polls until the pause at 7000.
    let events = playlist.apply(7_000, PlaylistCommand::Pause).unwrap();
    assert_eq!(
        entry_ons(&events),
        vec![(5_000, 2, 2_000, AdvanceCause::Duration)]
    );
    assert_eq!(
        events.last().unwrap(),
        &PlaylistEvent {
            at_ms: 7_000,
            kind: PlaylistEventKind::Paused {
                entry: id(2),
                take: 2,
                elapsed_ms: 2_000,
            },
        }
    );
    // Time stands still for B while paused, however long.
    assert_eq!(playlist.poll(100_000), Ok(Vec::new()));
    assert_eq!(
        playlist.apply(100_000, PlaylistCommand::Pause),
        Ok(Vec::new())
    );
    let events = playlist.apply(100_000, PlaylistCommand::Resume).unwrap();
    assert_eq!(
        events,
        vec![PlaylistEvent {
            at_ms: 100_000,
            kind: PlaylistEventKind::Resumed {
                entry: id(2),
                take: 2,
                elapsed_ms: 2_000,
            },
        }]
    );
    assert_eq!(
        playlist.apply(100_000, PlaylistCommand::Resume),
        Ok(Vec::new())
    );
    // B has 1000 ms left.
    assert_eq!(playlist.poll(100_999), Ok(Vec::new()));
    assert_eq!(
        entry_ons(&playlist.poll(101_000).unwrap()),
        vec![(101_000, 3, 0, AdvanceCause::Duration)]
    );
}

#[test]
fn pause_exactly_on_a_boundary_pauses_the_new_entry_at_zero() {
    let mut playlist = abc(LoopMode::Off);
    playlist.apply(0, start_now()).unwrap();
    let events = playlist.apply(5_000, PlaylistCommand::Pause).unwrap();
    assert_eq!(
        entry_ons(&events),
        vec![(5_000, 2, 0, AdvanceCause::Duration)]
    );
    assert_eq!(
        playlist.state(),
        PlaylistState::Paused {
            entry: id(2),
            take: 2,
            cycle: 0,
            elapsed_ms: 0,
        }
    );
}

#[test]
fn loop_all_wraps_with_cycle_numbers_and_collapses_long_gaps() {
    let mut playlist = abc(LoopMode::All);
    playlist.apply(0, start_now()).unwrap();
    let events = playlist.poll(10_000).unwrap();
    assert!(events.contains(&PlaylistEvent {
        at_ms: 10_000,
        kind: PlaylistEventKind::Looped {
            cycle: 1,
            skipped_cycles: 0,
        },
    }));
    assert_eq!(
        playlist.state(),
        PlaylistState::Playing {
            entry: id(1),
            take: 4,
            cycle: 1,
            started_at_ms: 10_000,
        }
    );

    // A gap of one million cycles yields a bounded number of events.
    let gap = 10_000 * 1_000_000;
    let events = playlist.poll(10_000 + gap + 6_000).unwrap();
    assert!(events.len() <= 3 * 3 + 4, "{events:?}");
    let looped: Vec<_> = events
        .iter()
        .filter_map(|event| match event.kind {
            PlaylistEventKind::Looped {
                cycle,
                skipped_cycles,
            } => Some((cycle, skipped_cycles)),
            _ => None,
        })
        .collect();
    assert_eq!(looped, vec![(1_000_001, 999_999)]);
    assert_eq!(
        entry_ons(&events),
        vec![(10_000 + gap + 5_000, 2, 1_000, AdvanceCause::Duration)]
    );
    match playlist.state() {
        PlaylistState::Playing { cycle, .. } => assert_eq!(cycle, 1_000_001),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn loop_one_repeats_the_on_air_entry_with_new_takes() {
    let mut playlist = abc(LoopMode::One);
    playlist.apply(0, start_now()).unwrap();
    let events = playlist.poll(5_000).unwrap();
    assert_eq!(
        entry_ons(&events),
        vec![(5_000, 1, 0, AdvanceCause::Duration)]
    );
    let events = playlist.poll(1_000_000_123).unwrap();
    assert_eq!(skipped(&events), Vec::<u64>::new());
    assert_eq!(
        entry_ons(&events),
        vec![(1_000_000_000, 1, 123, AdvanceCause::Duration)]
    );
    assert_eq!(on_air_id(&playlist), Some(1));
    // Disabling the looped entry lets the list move on at its end.
    playlist
        .apply(
            1_000_000_200,
            PlaylistCommand::SetEnabled {
                entry: id(1),
                enabled: false,
            },
        )
        .unwrap();
    assert_eq!(
        entry_ons(&playlist.poll(1_000_005_000).unwrap()),
        vec![(1_000_005_000, 2, 0, AdvanceCause::Duration)]
    );
}

#[test]
fn next_and_previous_wrap_only_in_loop_all() {
    let mut off = abc(LoopMode::Off);
    off.apply(0, start_now()).unwrap();
    assert_eq!(
        off.apply(10, PlaylistCommand::Previous),
        Err(PlaylistError::NoPreviousEntry)
    );
    assert_eq!(
        entry_ons(&off.apply(20, PlaylistCommand::Next).unwrap()),
        vec![(20, 2, 0, AdvanceCause::Next)]
    );
    off.apply(30, PlaylistCommand::Next).unwrap();
    assert_eq!(
        off.apply(40, PlaylistCommand::Next),
        Err(PlaylistError::NoNextEntry)
    );
    assert_eq!(on_air_id(&off), Some(3));
    assert_eq!(
        entry_ons(&off.apply(50, PlaylistCommand::Previous).unwrap()),
        vec![(50, 2, 0, AdvanceCause::Previous)]
    );

    let mut all = abc(LoopMode::All);
    all.apply(0, start_now()).unwrap();
    assert_eq!(
        entry_ons(&all.apply(10, PlaylistCommand::Previous).unwrap()),
        vec![(10, 3, 0, AdvanceCause::Previous)]
    );
    let events = all.apply(20, PlaylistCommand::Next).unwrap();
    assert_eq!(
        events[0].kind,
        PlaylistEventKind::Looped {
            cycle: 1,
            skipped_cycles: 0,
        }
    );
    assert_eq!(entry_ons(&events), vec![(20, 1, 0, AdvanceCause::Next)]);

    let mut one = abc(LoopMode::One);
    one.apply(0, start_now()).unwrap();
    one.apply(0, PlaylistCommand::JumpTo(id(3))).unwrap();
    assert_eq!(
        one.apply(10, PlaylistCommand::Next),
        Err(PlaylistError::NoNextEntry)
    );
}

#[test]
fn navigation_skips_disabled_entries_and_keeps_pause() {
    let mut playlist = Playlist::new(
        vec![
            fixed(1, 1_000),
            fixed(2, 1_000).with_enabled(false),
            fixed(3, 1_000),
            fixed(4, 1_000).with_enabled(false),
        ],
        config(LoopMode::Off, LatePolicy::Reject),
    )
    .unwrap();
    playlist.apply(0, start_now()).unwrap();
    playlist.apply(100, PlaylistCommand::Pause).unwrap();
    let events = playlist.apply(200, PlaylistCommand::Next).unwrap();
    assert_eq!(entry_ons(&events), vec![(200, 3, 0, AdvanceCause::Next)]);
    assert_eq!(
        events.last().unwrap().kind,
        PlaylistEventKind::Paused {
            entry: id(3),
            take: 2,
            elapsed_ms: 0,
        }
    );
    assert_eq!(
        playlist.apply(300, PlaylistCommand::Next),
        Err(PlaylistError::NoNextEntry)
    );
    assert_eq!(
        playlist.apply(300, PlaylistCommand::JumpTo(id(2))),
        Err(PlaylistError::EntryDisabled(id(2)))
    );
    assert_eq!(
        playlist.apply(300, PlaylistCommand::JumpTo(id(7))),
        Err(PlaylistError::UnknownEntry(id(7)))
    );
    assert_eq!(
        entry_ons(&playlist.apply(400, PlaylistCommand::Previous).unwrap()),
        vec![(400, 1, 0, AdvanceCause::Previous)]
    );
    // Still paused; resuming runs entry 1 for its full duration, then skips 2.
    assert_eq!(playlist.poll(10_000), Ok(Vec::new()));
    playlist.apply(10_000, PlaylistCommand::Resume).unwrap();
    assert_eq!(
        entry_ons(&playlist.poll(11_000).unwrap()),
        vec![(11_000, 3, 0, AdvanceCause::Duration)]
    );
    // The automatic end of entry 3 skips disabled entry 4 and finishes.
    assert_eq!(
        playlist.poll(12_000).unwrap()[0].kind,
        PlaylistEventKind::Finished { last: Some(id(3)) }
    );
}

#[test]
fn jump_to_puts_the_target_on_air() {
    let mut playlist = abc(LoopMode::Off);
    playlist.apply(0, start_now()).unwrap();
    let events = playlist
        .apply(1_000, PlaylistCommand::JumpTo(id(3)))
        .unwrap();
    assert_eq!(entry_ons(&events), vec![(1_000, 3, 0, AdvanceCause::Jump)]);
    let mut stopped = abc(LoopMode::Off);
    assert_eq!(
        stopped.apply(0, PlaylistCommand::JumpTo(id(1))),
        Err(PlaylistError::NotOnAir)
    );
}

#[test]
fn unknown_media_length_holds_until_media_ended_for_the_current_take() {
    let mut playlist = Playlist::new(
        vec![media(1, None), media(2, Some(4_000)), fixed(3, 1_000)],
        config(LoopMode::Off, LatePolicy::Reject),
    )
    .unwrap();
    playlist.apply(0, start_now()).unwrap();
    assert_eq!(playlist.poll(1_000_000), Ok(Vec::new()));
    assert_eq!(
        playlist.apply(1_000_000, PlaylistCommand::MediaEnded { take: 7 }),
        Err(PlaylistError::StaleTake {
            take: 7,
            current: Some(1),
        })
    );
    playlist.apply(1_000_000, PlaylistCommand::Pause).unwrap();
    assert_eq!(
        playlist.apply(1_000_000, PlaylistCommand::MediaEnded { take: 1 }),
        Err(PlaylistError::NotPlaying)
    );
    playlist.apply(1_000_000, PlaylistCommand::Resume).unwrap();
    let events = playlist
        .apply(1_000_500, PlaylistCommand::MediaEnded { take: 1 })
        .unwrap();
    assert_eq!(
        entry_ons(&events),
        vec![(1_000_500, 2, 0, AdvanceCause::MediaEnded)]
    );
    // A duplicate report for the previous take cannot skip entry 2.
    assert_eq!(
        playlist.apply(1_000_600, PlaylistCommand::MediaEnded { take: 1 }),
        Err(PlaylistError::StaleTake {
            take: 1,
            current: Some(2),
        })
    );
    // A known media length ends on time, and an early end report is honoured.
    assert_eq!(
        entry_ons(&playlist.poll(1_004_500).unwrap()),
        vec![(1_004_500, 3, 0, AdvanceCause::Duration)]
    );
    assert_eq!(
        playlist.apply(1_004_600, PlaylistCommand::MediaEnded { take: 3 }),
        Err(PlaylistError::NotMediaDuration(id(3)))
    );
}

#[test]
fn catch_up_stops_at_an_entry_of_unknown_length() {
    let mut playlist = Playlist::new(
        vec![fixed(1, 1_000), media(2, None), fixed(3, 1_000)],
        config(LoopMode::All, LatePolicy::Reject),
    )
    .unwrap();
    playlist.apply(0, start_now()).unwrap();
    let events = playlist.poll(1_000_000).unwrap();
    assert_eq!(
        entry_ons(&events),
        vec![(1_000, 2, 999_000, AdvanceCause::Duration)]
    );
    assert_eq!(on_air_id(&playlist), Some(2));
}

#[test]
fn errors_change_nothing_and_never_drop_boundary_events() {
    let mut playlist = abc(LoopMode::Off);
    playlist.apply(0, start_now()).unwrap();
    let before = playlist.snapshot();
    // At 9000 A and B have ended; Previous from C is valid, but JumpTo an
    // unknown entry is not. The failed call must not swallow the boundaries.
    assert_eq!(
        playlist.apply(9_000, PlaylistCommand::JumpTo(id(42))),
        Err(PlaylistError::UnknownEntry(id(42)))
    );
    assert_eq!(playlist.snapshot(), before);
    let events = playlist.poll(9_000).unwrap();
    assert_eq!(skipped(&events), vec![2]);
    assert_eq!(
        entry_ons(&events),
        vec![(8_000, 3, 1_000, AdvanceCause::Duration)]
    );
}

#[test]
fn clock_regression_and_out_of_range_times_are_rejected() {
    let mut playlist = abc(LoopMode::Off);
    playlist.apply(5_000, start_now()).unwrap();
    let before = playlist.snapshot();
    assert_eq!(
        playlist.poll(4_999),
        Err(PlaylistError::ClockRegressed {
            observed_ms: 5_000,
            now_ms: 4_999,
        })
    );
    assert_eq!(
        playlist.poll(MAX_PLAYLIST_TIME_MS + 1),
        Err(PlaylistError::TimeOutOfRange {
            ms: MAX_PLAYLIST_TIME_MS + 1,
            max: MAX_PLAYLIST_TIME_MS,
        })
    );
    assert_eq!(playlist.snapshot(), before);
    // Equal time is allowed and idempotent.
    assert_eq!(playlist.poll(5_000), Ok(Vec::new()));

    let mut far = abc(LoopMode::Off);
    assert!(matches!(
        far.apply(
            10,
            PlaylistCommand::Start {
                at: StartAt::After { delay_ms: u64::MAX },
                from: None,
            }
        ),
        Err(PlaylistError::TimeOutOfRange { .. })
    ));
}

#[test]
fn stop_and_restart_after_finish_resets_the_cycle() {
    let mut playlist = abc(LoopMode::Off);
    playlist.apply(0, start_now()).unwrap();
    playlist.poll(20_000).unwrap();
    assert_eq!(
        playlist.state(),
        PlaylistState::Finished { last: Some(id(3)) }
    );
    let events = playlist.apply(20_000, start_now()).unwrap();
    assert_eq!(
        events[1].kind,
        PlaylistEventKind::EntryOn {
            entry: id(1),
            input: input(101),
            transition: PlaylistTransition::CUT,
            take: 4,
            cycle: 0,
            offset_ms: 0,
            cause: AdvanceCause::Start,
        }
    );
    assert_eq!(
        playlist.apply(21_000, PlaylistCommand::Stop).unwrap(),
        vec![PlaylistEvent {
            at_ms: 21_000,
            kind: PlaylistEventKind::Stopped,
        }]
    );
    assert_eq!(
        playlist.apply(21_000, PlaylistCommand::Stop),
        Ok(Vec::new())
    );
    assert_eq!(playlist.poll(100_000), Ok(Vec::new()));
}

#[test]
fn entry_and_transition_validation() {
    let cfg = PlaylistConfig::default();
    assert_eq!(
        Playlist::new(vec![fixed(1, 1_000), fixed(1, 2_000)], cfg),
        Err(PlaylistError::DuplicateEntry(id(1)))
    );
    assert_eq!(
        Playlist::new(vec![fixed(1, 0)], cfg),
        Err(PlaylistError::ZeroDuration(id(1)))
    );
    assert_eq!(
        Playlist::new(vec![media(1, Some(0))], cfg),
        Err(PlaylistError::ZeroDuration(id(1)))
    );
    assert_eq!(
        Playlist::new(vec![fixed(1, MAX_PLAYLIST_ENTRY_MS + 1)], cfg),
        Err(PlaylistError::DurationTooLong {
            entry: id(1),
            ms: MAX_PLAYLIST_ENTRY_MS + 1,
            max: MAX_PLAYLIST_ENTRY_MS,
        })
    );
    assert_eq!(
        Playlist::new(
            vec![
                fixed(1, 1_000)
                    .with_transition(PlaylistTransition::new(PlaylistTransitionKind::Cut, 10))
            ],
            cfg
        ),
        Err(PlaylistError::InvalidTransition(id(1)))
    );
    assert_eq!(
        Playlist::new(
            vec![
                fixed(1, 1_000)
                    .with_transition(PlaylistTransition::new(PlaylistTransitionKind::Wipe, 0))
            ],
            cfg
        ),
        Err(PlaylistError::InvalidTransition(id(1)))
    );
    assert_eq!(
        Playlist::new(
            vec![fixed(1, 1_000).with_transition(PlaylistTransition::new(
                PlaylistTransitionKind::Stinger,
                1_001
            ))],
            cfg
        ),
        Err(PlaylistError::TransitionLongerThanEntry {
            entry: id(1),
            transition_ms: 1_001,
            duration_ms: 1_000,
        })
    );
    let too_many = (0..=u64::try_from(MAX_PLAYLIST_ENTRIES).unwrap())
        .map(|entry| fixed(entry, 1_000))
        .collect();
    assert_eq!(
        Playlist::new(too_many, cfg),
        Err(PlaylistError::TooManyEntries {
            count: MAX_PLAYLIST_ENTRIES + 1,
            max: MAX_PLAYLIST_ENTRIES,
        })
    );
    // Unknown media length accepts any transition up to the limit.
    assert!(
        Playlist::new(
            vec![
                media(1, None)
                    .with_transition(PlaylistTransition::new(PlaylistTransitionKind::Fade, 2_000))
            ],
            cfg
        )
        .is_ok()
    );
}

#[test]
fn entries_are_replaceable_only_while_inactive() {
    let mut playlist = abc(LoopMode::Off);
    playlist.apply(0, start_now()).unwrap();
    assert_eq!(
        playlist.replace_entries(vec![fixed(9, 1_000)]),
        Err(PlaylistError::EditWhileActive)
    );
    playlist.apply(1, PlaylistCommand::Stop).unwrap();
    playlist.replace_entries(vec![fixed(9, 1_000)]).unwrap();
    assert_eq!(playlist.entries().len(), 1);
    assert_eq!(
        playlist.apply(
            2,
            PlaylistCommand::SetEnabled {
                entry: id(1),
                enabled: false,
            }
        ),
        Err(PlaylistError::UnknownEntry(id(1)))
    );
}

/// Runs the same command script against two playlists and asserts identical
/// events and final snapshots.
fn assert_same_future(
    mut left: Playlist,
    mut right: Playlist,
    script: &[(u64, Option<PlaylistCommand>)],
) {
    for (now, command) in script {
        let (a, b) = match command {
            Some(command) => (left.apply(*now, *command), right.apply(*now, *command)),
            None => (left.poll(*now), right.poll(*now)),
        };
        assert_eq!(a, b, "diverged at {now}");
    }
    assert_eq!(left.snapshot(), right.snapshot());
}

#[test]
fn snapshot_round_trips_through_text_and_restores_identical_behavior() {
    let mut playlist = Playlist::new(
        vec![
            fixed(1, 5_000),
            media(2, None).with_transition(PlaylistTransition::new(
                PlaylistTransitionKind::AlphaFade,
                750,
            )),
            media(3, Some(2_500)).with_enabled(false),
            fixed(4, 3_000)
                .with_transition(PlaylistTransition::new(PlaylistTransitionKind::Zoom, 1)),
        ],
        config(LoopMode::All, LatePolicy::JoinInProgress),
    )
    .unwrap();
    let script: Vec<(u64, Option<PlaylistCommand>)> = vec![
        (100_000, None),
        (100_100, Some(PlaylistCommand::MediaEnded { take: 3 })),
        (102_000, Some(PlaylistCommand::Pause)),
        (150_000, Some(PlaylistCommand::Resume)),
        (160_000, None),
        (
            160_000,
            Some(PlaylistCommand::SetEnabled {
                entry: id(3),
                enabled: true,
            }),
        ),
        (160_001, Some(PlaylistCommand::Previous)),
        (170_000, None),
        (170_000, Some(PlaylistCommand::SetLoopMode(LoopMode::Off))),
        (200_000, None),
    ];
    let mut states = Vec::new();
    // Capture every reachable state kind along the way.
    states.push(playlist.snapshot());
    playlist
        .apply(
            1_000,
            PlaylistCommand::Start {
                at: StartAt::At { wall_ms: 90_000 },
                from: Some(id(4)),
            },
        )
        .unwrap();
    states.push(playlist.snapshot());
    playlist.poll(95_000).unwrap();
    states.push(playlist.snapshot());
    playlist.apply(96_000, PlaylistCommand::Pause).unwrap();
    states.push(playlist.snapshot());
    playlist.apply(96_000, PlaylistCommand::Resume).unwrap();
    states.push(playlist.snapshot());

    for snapshot in &states {
        let text = encode_playlist_snapshot(snapshot);
        let decoded = decode_playlist_snapshot(&text).unwrap();
        assert_eq!(&decoded, snapshot, "{text}");
        assert_eq!(encode_playlist_snapshot(&decoded), text);
        let restored = Playlist::restore(decoded).unwrap();
        assert_eq!(&restored.snapshot(), snapshot);
    }
    // The live playlist and one restored from text behave identically.
    let restored = Playlist::restore(
        decode_playlist_snapshot(&encode_playlist_snapshot(&playlist.snapshot())).unwrap(),
    )
    .unwrap();
    assert_same_future(playlist.clone(), restored, &script);

    // A finished snapshot round-trips too.
    let mut finished = abc(LoopMode::Off);
    finished.apply(0, start_now()).unwrap();
    finished.poll(10_000).unwrap();
    let text = encode_playlist_snapshot(&finished.snapshot());
    assert!(text.contains("state\tfinished\t3\n"), "{text}");
    assert_eq!(
        Playlist::restore(decode_playlist_snapshot(&text).unwrap()).unwrap(),
        finished
    );
}

#[test]
fn restored_playing_state_catches_up_downtime_on_the_next_call() {
    let mut playlist = abc(LoopMode::Off);
    playlist.apply(0, start_now()).unwrap();
    playlist.poll(1_000).unwrap();
    let restored = Playlist::restore(playlist.snapshot()).unwrap();
    let mut restored = restored;
    // The process was down from 1000 to 6500.
    let events = restored.poll(6_500).unwrap();
    assert_eq!(
        entry_ons(&events),
        vec![(5_000, 2, 1_500, AdvanceCause::Duration)]
    );
}

fn base_snapshot(state: PlaylistState) -> PlaylistSnapshot {
    PlaylistSnapshot {
        entries: vec![fixed(1, 5_000), media(2, None)],
        config: PlaylistConfig::default(),
        observed_ms: 10_000,
        next_take: 5,
        state,
    }
}

#[test]
#[allow(clippy::too_many_lines)] // one table of rejected snapshots
fn restore_rejects_inconsistent_state() {
    let playing = |entry, take, started_at_ms| PlaylistState::Playing {
        entry: id(entry),
        take,
        cycle: 0,
        started_at_ms,
    };
    let paused = |entry, take, elapsed_ms| PlaylistState::Paused {
        entry: id(entry),
        take,
        cycle: 0,
        elapsed_ms,
    };
    let cases = [
        (
            base_snapshot(playing(9, 1, 9_000)),
            SnapshotError::UnknownEntry(id(9)),
        ),
        (
            base_snapshot(playing(1, 5, 9_000)),
            SnapshotError::UnissuedTake {
                take: 5,
                next_take: 5,
            },
        ),
        (
            base_snapshot(playing(1, 0, 9_000)),
            SnapshotError::UnissuedTake {
                take: 0,
                next_take: 5,
            },
        ),
        (
            base_snapshot(playing(1, 4, 10_001)),
            SnapshotError::TimeAfterObserved {
                ms: 10_001,
                observed_ms: 10_000,
            },
        ),
        (
            base_snapshot(playing(1, 4, 5_000)),
            SnapshotError::BoundaryNotProcessed {
                entry: id(1),
                end_ms: 10_000,
                observed_ms: 10_000,
            },
        ),
        (
            base_snapshot(paused(1, 4, 5_000)),
            SnapshotError::ElapsedBeyondDuration {
                entry: id(1),
                elapsed_ms: 5_000,
                duration_ms: 5_000,
            },
        ),
        (
            base_snapshot(paused(2, 4, 10_001)),
            SnapshotError::TimeAfterObserved {
                ms: 10_001,
                observed_ms: 10_000,
            },
        ),
        (
            base_snapshot(PlaylistState::Armed {
                start_at_ms: 10_000,
                from: None,
            }),
            SnapshotError::ArmedStartPassed {
                start_at_ms: 10_000,
                observed_ms: 10_000,
            },
        ),
        (
            base_snapshot(PlaylistState::Armed {
                start_at_ms: 20_000,
                from: Some(id(3)),
            }),
            SnapshotError::UnknownEntry(id(3)),
        ),
        (
            base_snapshot(PlaylistState::Finished { last: Some(id(8)) }),
            SnapshotError::UnknownEntry(id(8)),
        ),
        (
            PlaylistSnapshot {
                next_take: 0,
                ..base_snapshot(PlaylistState::Stopped)
            },
            SnapshotError::ZeroNextTake,
        ),
        (
            PlaylistSnapshot {
                observed_ms: MAX_PLAYLIST_TIME_MS + 1,
                ..base_snapshot(PlaylistState::Stopped)
            },
            SnapshotError::TimeOutOfRange {
                ms: MAX_PLAYLIST_TIME_MS + 1,
            },
        ),
        (
            PlaylistSnapshot {
                entries: vec![fixed(1, 1_000), fixed(1, 1_000)],
                ..base_snapshot(PlaylistState::Stopped)
            },
            SnapshotError::Entries(PlaylistError::DuplicateEntry(id(1))),
        ),
        (
            PlaylistSnapshot {
                entries: Vec::new(),
                ..base_snapshot(PlaylistState::Armed {
                    start_at_ms: 20_000,
                    from: None,
                })
            },
            SnapshotError::Empty,
        ),
    ];
    for (snapshot, expected) in cases {
        assert_eq!(
            Playlist::restore(snapshot.clone()),
            Err(expected),
            "{snapshot:?}"
        );
    }
    // Consistent edge states are accepted: unknown-length media playing for a
    // long time, and a disabled entry on air.
    assert!(Playlist::restore(base_snapshot(playing(2, 4, 0))).is_ok());
    let mut disabled = base_snapshot(playing(1, 4, 9_000));
    disabled.entries[0].enabled = false;
    assert!(Playlist::restore(disabled).is_ok());
}

#[test]
fn snapshot_decoder_rejects_malformed_text() {
    let good = encode_playlist_snapshot(&base_snapshot(PlaylistState::Stopped));
    assert!(decode_playlist_snapshot(&good).is_ok());
    let cases = [
        ("", 1),
        ("freemix-shortcuts\t1\n", 1),
        ("freemix-playlist\t2\n", 1),
        ("freemix-playlist\t1\nconfig\tsometimes\treject\n", 2),
        ("freemix-playlist\t1\nconfig\toff\treject\n", 3),
        (
            "freemix-playlist\t1\nconfig\toff\treject\nclock\t-1\t1\nstate\tstopped\n",
            3,
        ),
        (
            "freemix-playlist\t1\nconfig\toff\treject\nclock\t0\t1\nstate\tdancing\n",
            4,
        ),
        (
            "freemix-playlist\t1\nconfig\toff\treject\nclock\t0\t1\nstate\tstopped\nentry\t1\t0\tfixed\t10\tcut\t0\ton\n",
            5,
        ),
        (
            "freemix-playlist\t1\nconfig\toff\treject\nclock\t0\t1\nstate\tstopped\nentry\t1\t5\tfixed\t-\tcut\t0\ton\n",
            5,
        ),
        (
            "freemix-playlist\t1\nconfig\toff\treject\nclock\t0\t1\nstate\tstopped\nentry\t1\t5\tfixed\t10\tspin\t0\ton\n",
            5,
        ),
        (
            "freemix-playlist\t1\nconfig\toff\treject\nclock\t0\t1\nstate\tstopped\nentry\t1\t5\tfixed\t10\tcut\t0\tyes\n",
            5,
        ),
        (
            "freemix-playlist\t1\nconfig\toff\treject\nclock\t0\t1\nstate\tstopped\nentry\t1\t5\tfixed\t10\tcut\t0\ton\textra\n",
            5,
        ),
    ];
    for (text, line) in cases {
        let error = decode_playlist_snapshot(text).unwrap_err();
        assert_eq!(error.line, line, "{text:?}: {error}");
    }
}

#[test]
fn pseudo_random_poll_cadences_reach_identical_state_in_every_loop_mode() {
    for loop_mode in [LoopMode::Off, LoopMode::All, LoopMode::One] {
        for seed in 1_u64..=20 {
            let entries = vec![
                fixed(1, 700),
                fixed(2, 1_300).with_enabled(false),
                fixed(3, 250),
                media(4, Some(900)),
                fixed(5, 1),
            ];
            let cfg = config(loop_mode, LatePolicy::Reject);
            let mut reference = Playlist::new(entries.clone(), cfg).unwrap();
            let mut sampled = Playlist::new(entries, cfg).unwrap();
            reference.apply(17, start_now()).unwrap();
            sampled.apply(17, start_now()).unwrap();
            let mut state = seed;
            let mut now = 17;
            while now < 40_000 {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                now += (state >> 33) % 2_500;
                sampled.poll(now).unwrap();
            }
            for tick in 18..=now {
                reference.poll(tick).unwrap();
            }
            assert_eq!(
                reference.snapshot(),
                sampled.snapshot(),
                "{loop_mode:?} seed {seed}"
            );
        }
    }
}
