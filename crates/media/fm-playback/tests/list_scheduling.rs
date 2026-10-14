//! Integration coverage for list inputs: ordered auto-advance, loop, Auto
//! First, per-item durations/marks, deterministic shuffle, and frame-exact
//! programmed GO at absolute and relative output-frame coordinates.

use fm_playback::{
    ActivationCause, CancelGoOutcome, ClipId, ClipLibrary, FixtureClip, FrameIndex, GoId,
    GoScheduleOutcome, GoStatus, LibraryError, ListAction, ListControlError, ListError, ListEvent,
    ListGo, ListGoError, ListItem, ListItemId, ListOptions, ListPlayer, ListTickError,
    MAX_LIST_ITEMS, MAX_PENDING_LIST_GO, Marks, MediaList, PlaybackError, ScheduleCoordinate,
};

const A: ListItemId = ListItemId::new(1);
const B: ListItemId = ListItemId::new(2);
const C: ListItemId = ListItemId::new(3);

fn library() -> ClipLibrary<u8> {
    let mut library = ClipLibrary::default();
    for (id, frames) in [(1, vec![10, 11, 12]), (2, vec![20, 21]), (3, vec![30])] {
        library
            .insert(FixtureClip::new(ClipId::new(id), frames).unwrap().into())
            .unwrap();
    }
    library
}

fn abc() -> Vec<ListItem> {
    vec![
        ListItem::new(A, ClipId::new(1)),
        ListItem::new(B, ClipId::new(2)),
        ListItem::new(C, ClipId::new(3)),
    ]
}

fn player(items: Vec<ListItem>, options: ListOptions) -> ListPlayer<u8> {
    ListPlayer::new(library(), MediaList::new(items, options).unwrap()).unwrap()
}

fn auto_next() -> ListOptions {
    ListOptions {
        auto_next: true,
        ..ListOptions::default()
    }
}

fn at(value: u64) -> ScheduleCoordinate {
    ScheduleCoordinate::new(value)
}

/// Ticks `count` consecutive coordinates starting at `start`, returning the
/// emitted frame values (`None` while idle).
fn run(player: &mut ListPlayer<u8>, start: u64, count: u64) -> Vec<Option<u8>> {
    (start..start + count)
        .map(|coordinate| {
            player
                .tick(at(coordinate), None)
                .unwrap()
                .frame
                .map(|frame| frame.frame)
        })
        .collect()
}

fn values(frames: &[u8]) -> Vec<Option<u8>> {
    frames.iter().copied().map(Some).collect()
}

#[test]
fn idle_list_emits_no_frames_until_activated() {
    let mut player = player(abc(), auto_next());
    let tick = player.tick(at(0), None).unwrap();
    assert_eq!(tick.frame, None);
    assert_eq!(tick.item, None);
    assert_eq!(tick.events, Vec::new());
    assert_eq!(player.current_item(), None);
    assert_eq!(
        player.control(ListAction::Next),
        Err(ListControlError::NoActiveItem)
    );
    assert_eq!(player.pause(), Err(ListControlError::NoActiveItem));
}

#[test]
fn auto_next_is_gapless_and_holds_after_the_last_item() {
    let mut player = player(abc(), auto_next());
    player.control(ListAction::First).unwrap();
    assert_eq!(
        run(&mut player, 0, 8),
        values(&[10, 11, 12, 20, 21, 30, 30, 30])
    );
    assert!(!player.is_playing());
    assert_eq!(player.current_item(), Some(C));
}

#[test]
fn item_end_events_name_the_successor_and_its_cause() {
    let mut player = player(abc(), auto_next());
    player.control(ListAction::First).unwrap();
    run(&mut player, 0, 2);
    let tick = player.tick(at(2), None).unwrap();
    let frame = tick.frame.unwrap();
    assert_eq!(frame.index, FrameIndex::new(2));
    assert!(frame.ended);
    assert_eq!(tick.item, Some(A));
    assert_eq!(
        tick.events,
        vec![
            ListEvent::ItemEnded { item: A },
            ListEvent::ItemActivated {
                item: B,
                position: 1,
                cause: ActivationCause::AutoNext,
            },
        ]
    );
    run(&mut player, 3, 2);
    let last = player.tick(at(5), None).unwrap();
    assert_eq!(
        last.events,
        vec![
            ListEvent::ItemEnded { item: C },
            ListEvent::ListCompleted { cycle: 0 },
        ]
    );
}

#[test]
fn without_auto_next_each_item_holds_its_final_frame() {
    let mut player = player(abc(), ListOptions::default());
    player.control(ListAction::First).unwrap();
    assert_eq!(run(&mut player, 0, 5), values(&[10, 11, 12, 12, 12]));
    assert_eq!(player.current_item(), Some(A));
    assert!(!player.is_playing());
}

#[test]
fn loop_list_wraps_and_counts_cycles() {
    let options = ListOptions {
        loop_list: true,
        ..auto_next()
    };
    let mut player = player(abc(), options);
    player.control(ListAction::First).unwrap();
    assert_eq!(
        run(&mut player, 0, 12),
        values(&[10, 11, 12, 20, 21, 30, 10, 11, 12, 20, 21, 30])
    );
    assert_eq!(player.cycle(), 2);
    let tick = player.tick(at(12), None).unwrap();
    assert_eq!(tick.frame.unwrap().frame, 10);
    assert_eq!(player.cycle(), 2);
}

#[test]
fn loop_wrap_reports_completion_then_loop_activation() {
    let options = ListOptions {
        loop_list: true,
        ..auto_next()
    };
    let mut player = player(abc(), options);
    player.control(ListAction::Play(C)).unwrap();
    let tick = player.tick(at(0), None).unwrap();
    assert_eq!(
        tick.events,
        vec![
            ListEvent::ItemEnded { item: C },
            ListEvent::ListCompleted { cycle: 0 },
            ListEvent::ItemActivated {
                item: A,
                position: 0,
                cause: ActivationCause::Loop,
            },
        ]
    );
}

#[test]
fn auto_first_cues_the_first_item_paused() {
    let options = ListOptions {
        auto_first: true,
        ..auto_next()
    };
    let mut player = player(abc(), options);
    player.control(ListAction::Play(B)).unwrap();
    assert_eq!(run(&mut player, 0, 6), values(&[20, 21, 30, 10, 10, 10]));
    assert_eq!(player.current_item(), Some(A));
    assert!(!player.is_playing());
    player.resume().unwrap();
    assert_eq!(run(&mut player, 6, 3), values(&[10, 11, 12]));
}

#[test]
fn durations_cut_ranges_and_hold_stills_exactly() {
    let items = vec![
        ListItem::new(A, ClipId::new(1)).with_duration(2),
        ListItem::new(C, ClipId::new(3)).with_duration(4),
        ListItem::new(B, ClipId::new(2)),
    ];
    let mut player = player(items, auto_next());
    player.control(ListAction::First).unwrap();
    assert_eq!(
        run(&mut player, 0, 8),
        values(&[10, 11, 30, 30, 30, 30, 20, 21])
    );
}

#[test]
fn marks_select_an_inclusive_in_out_range() {
    let marks = Marks::new(FrameIndex::new(1), FrameIndex::new(2)).unwrap();
    let items = vec![
        ListItem::new(A, ClipId::new(1)).with_marks(marks),
        ListItem::new(B, ClipId::new(2)),
    ];
    let mut player = player(items, auto_next());
    player.control(ListAction::First).unwrap();
    assert_eq!(run(&mut player, 0, 4), values(&[11, 12, 20, 21]));
}

#[test]
fn pause_holds_without_consuming_duration() {
    let items = vec![
        ListItem::new(A, ClipId::new(1)).with_duration(3),
        ListItem::new(B, ClipId::new(2)),
    ];
    let mut player = player(items, auto_next());
    player.control(ListAction::First).unwrap();
    assert_eq!(run(&mut player, 0, 1), values(&[10]));
    player.pause().unwrap();
    assert_eq!(run(&mut player, 1, 3), values(&[11, 11, 11]));
    assert_eq!(player.elapsed_frames(), Some(1));
    player.resume().unwrap();
    assert_eq!(run(&mut player, 4, 4), values(&[11, 12, 20, 21]));
}

#[test]
fn manual_next_previous_and_wrap_rules() {
    let mut linear = player(abc(), auto_next());
    linear.control(ListAction::Play(C)).unwrap();
    assert_eq!(
        linear.control(ListAction::Next),
        Err(ListControlError::EndOfList)
    );
    linear.control(ListAction::Previous).unwrap();
    assert_eq!(linear.current_item(), Some(B));
    linear.control(ListAction::First).unwrap();
    linear.control(ListAction::Previous).unwrap();
    assert_eq!(linear.current_item(), Some(A));

    let mut looping = player(
        abc(),
        ListOptions {
            loop_list: true,
            ..auto_next()
        },
    );
    looping.control(ListAction::First).unwrap();
    looping.control(ListAction::Previous).unwrap();
    assert_eq!(looping.current_item(), Some(C));
    assert_eq!(looping.cycle(), 0);
    looping.control(ListAction::Next).unwrap();
    assert_eq!(looping.current_item(), Some(A));
    assert_eq!(looping.cycle(), 1);
    assert_eq!(
        looping.control(ListAction::Play(ListItemId::new(99))),
        Err(ListControlError::MissingItem(ListItemId::new(99)))
    );
}

#[test]
fn programmed_go_lands_on_the_exact_coordinate() {
    let mut player = player(abc(), ListOptions::default());
    player
        .schedule_go(ListGo {
            id: GoId::new(1),
            coordinate: at(3),
            action: ListAction::Play(B),
        })
        .unwrap();
    assert_eq!(
        run(&mut player, 0, 3),
        vec![None, None, None],
        "nothing plays before the GO"
    );
    let tick = player.tick(at(3), None).unwrap();
    assert_eq!(tick.frame.unwrap().frame, 20);
    assert_eq!(tick.item, Some(B));
    assert_eq!(
        tick.events,
        vec![
            ListEvent::GoExecuted {
                go: GoId::new(1),
                scheduled: at(3),
            },
            ListEvent::ItemActivated {
                item: B,
                position: 1,
                cause: ActivationCause::Go(GoId::new(1)),
            },
        ]
    );
    assert_eq!(player.go_status(GoId::new(1)), Some(GoStatus::Executed));
}

#[test]
fn programmed_go_cuts_a_playing_item_mid_range() {
    let mut player = player(abc(), auto_next());
    player.control(ListAction::First).unwrap();
    player
        .schedule_go(ListGo {
            id: GoId::new(7),
            coordinate: at(1),
            action: ListAction::Play(C),
        })
        .unwrap();
    assert_eq!(run(&mut player, 0, 3), values(&[10, 30, 30]));
}

#[test]
fn same_coordinate_gos_apply_in_stable_id_order() {
    let mut player = player(abc(), ListOptions::default());
    // Scheduled in reverse ID order; execution must still be by GoId.
    for (id, item) in [(9, A), (4, C), (6, B)] {
        player
            .schedule_go(ListGo {
                id: GoId::new(id),
                coordinate: at(2),
                action: ListAction::Play(item),
            })
            .unwrap();
    }
    assert_eq!(
        player.pending_go(),
        vec![
            (at(2), GoId::new(4)),
            (at(2), GoId::new(6)),
            (at(2), GoId::new(9))
        ]
    );
    run(&mut player, 0, 2);
    let tick = player.tick(at(2), None).unwrap();
    let executed: Vec<_> = tick
        .events
        .iter()
        .filter_map(|event| match event {
            ListEvent::GoExecuted { go, .. } => Some(go.get()),
            _ => None,
        })
        .collect();
    assert_eq!(executed, vec![4, 6, 9]);
    assert_eq!(tick.item, Some(A));
}

#[test]
fn go_scheduling_is_idempotent_cancellable_and_validated() {
    let mut player = player(abc(), ListOptions::default());
    let go = ListGo {
        id: GoId::new(1),
        coordinate: at(5),
        action: ListAction::First,
    };
    assert_eq!(player.schedule_go(go), Ok(GoScheduleOutcome::Scheduled));
    assert_eq!(
        player.schedule_go(go),
        Ok(GoScheduleOutcome::Unchanged(GoStatus::Pending))
    );
    assert_eq!(player.pending_go_count(), 1);
    assert_eq!(
        player.schedule_go(ListGo {
            coordinate: at(6),
            ..go
        }),
        Err(ListGoError::ConflictingGo(GoId::new(1)))
    );
    assert_eq!(
        player.schedule_go(ListGo {
            id: GoId::new(2),
            coordinate: at(5),
            action: ListAction::Play(ListItemId::new(42)),
        }),
        Err(ListGoError::MissingItem(ListItemId::new(42)))
    );
    assert_eq!(
        player.cancel_go(GoId::new(1)),
        Ok(CancelGoOutcome::Cancelled)
    );
    assert_eq!(
        player.cancel_go(GoId::new(1)),
        Ok(CancelGoOutcome::AlreadyCancelled)
    );
    assert_eq!(
        player.schedule_go(go),
        Ok(GoScheduleOutcome::Unchanged(GoStatus::Cancelled))
    );
    assert_eq!(
        player.cancel_go(GoId::new(3)),
        Err(ListGoError::UnknownGo(GoId::new(3)))
    );
    assert_eq!(
        run(&mut player, 0, 7),
        vec![None; 7],
        "cancelled GO never fires"
    );

    player.tick(at(10), None).unwrap();
    for coordinate in [0, 10] {
        assert_eq!(
            player.schedule_go(ListGo {
                id: GoId::new(5),
                coordinate: at(coordinate),
                action: ListAction::First,
            }),
            Err(ListGoError::CoordinateElapsed(at(coordinate)))
        );
    }
}

#[test]
fn executed_go_reports_terminal_status_on_retry_and_cancel() {
    let mut player = player(abc(), ListOptions::default());
    let go = ListGo {
        id: GoId::new(1),
        coordinate: at(0),
        action: ListAction::First,
    };
    player.schedule_go(go).unwrap();
    player.tick(at(0), None).unwrap();
    assert_eq!(
        player.schedule_go(go),
        Ok(GoScheduleOutcome::Unchanged(GoStatus::Executed))
    );
    assert_eq!(
        player.cancel_go(GoId::new(1)),
        Ok(CancelGoOutcome::AlreadyExecuted)
    );
}

#[test]
fn inapplicable_go_is_consumed_and_reported() {
    let mut player = player(abc(), ListOptions::default());
    for (id, coordinate, action) in [
        (1, 0, ListAction::Next),
        (2, 1, ListAction::Play(C)),
        (3, 2, ListAction::Next),
    ] {
        player
            .schedule_go(ListGo {
                id: GoId::new(id),
                coordinate: at(coordinate),
                action,
            })
            .unwrap();
    }
    let first = player.tick(at(0), None).unwrap();
    assert_eq!(
        first.events,
        vec![
            ListEvent::GoExecuted {
                go: GoId::new(1),
                scheduled: at(0),
            },
            ListEvent::GoIgnored {
                go: GoId::new(1),
                reason: ListControlError::NoActiveItem,
            },
        ]
    );
    player.tick(at(1), None).unwrap();
    let third = player.tick(at(2), None).unwrap();
    assert_eq!(
        third.events[1],
        ListEvent::GoIgnored {
            go: GoId::new(3),
            reason: ListControlError::EndOfList,
        }
    );
    assert_eq!(player.go_status(GoId::new(3)), Some(GoStatus::Executed));
    assert_eq!(player.current_item(), Some(C));
}

#[test]
fn stop_go_rewinds_and_reports() {
    let mut player = player(abc(), auto_next());
    player.control(ListAction::First).unwrap();
    player
        .schedule_go(ListGo {
            id: GoId::new(1),
            coordinate: at(2),
            action: ListAction::Stop,
        })
        .unwrap();
    assert_eq!(run(&mut player, 0, 2), values(&[10, 11]));
    let tick = player.tick(at(2), None).unwrap();
    assert_eq!(tick.frame.unwrap().frame, 10);
    assert_eq!(tick.events[1], ListEvent::Stopped { item: A });
    assert!(!player.is_playing());
    assert_eq!(run(&mut player, 3, 2), values(&[10, 10]));
}

#[test]
fn relative_go_resolves_against_the_last_tick() {
    let mut player = player(abc(), auto_next());
    let (before_start, _) = player
        .schedule_go_in(GoId::new(1), 2, ListAction::First)
        .unwrap();
    assert_eq!(before_start.coordinate, at(2));
    run(&mut player, 0, 5);
    let (resolved, outcome) = player
        .schedule_go_in(GoId::new(2), 3, ListAction::Play(C))
        .unwrap();
    assert_eq!(resolved.coordinate, at(7));
    assert_eq!(outcome, GoScheduleOutcome::Scheduled);
    assert_eq!(
        player.schedule_go(resolved),
        Ok(GoScheduleOutcome::Unchanged(GoStatus::Pending))
    );
    assert_eq!(
        player.schedule_go_in(GoId::new(3), 0, ListAction::First),
        Err(ListGoError::CoordinateElapsed(at(4)))
    );
    assert_eq!(run(&mut player, 5, 3), values(&[20, 21, 30]));

    player.tick(at(u64::MAX - 1), None).unwrap();
    assert_eq!(
        player.schedule_go_in(GoId::new(4), 2, ListAction::First),
        Err(ListGoError::CoordinateOverflow)
    );
}

#[test]
fn ticks_must_advance_and_gaps_apply_late_gos_once() {
    let mut player = player(abc(), ListOptions::default());
    player
        .schedule_go(ListGo {
            id: GoId::new(1),
            coordinate: at(3),
            action: ListAction::Play(B),
        })
        .unwrap();
    player.tick(at(1), None).unwrap();
    for requested in [0, 1] {
        assert_eq!(
            player.tick(at(requested), None),
            Err(ListTickError::CoordinateNotAdvanced {
                previous: at(1),
                requested: at(requested),
            })
        );
    }
    let tick = player.tick(at(10), None).unwrap();
    assert_eq!(
        tick.events[0],
        ListEvent::GoExecuted {
            go: GoId::new(1),
            scheduled: at(3),
        }
    );
    assert_eq!(tick.frame.unwrap().frame, 20);
    let next = player.tick(at(11), None).unwrap();
    assert_eq!(next.events, vec![ListEvent::ItemEnded { item: B }]);
    assert_eq!(player.last_coordinate(), Some(at(11)));
}

fn shuffled_list(seed: u64) -> ListPlayer<u8> {
    let mut library = ClipLibrary::default();
    let mut items = Vec::new();
    for id in 0..8u8 {
        library
            .insert(
                FixtureClip::new(ClipId::new(u128::from(id)), vec![id])
                    .unwrap()
                    .into(),
            )
            .unwrap();
        items.push(ListItem::new(
            ListItemId::new(u128::from(id)),
            ClipId::new(u128::from(id)),
        ));
    }
    let options = ListOptions {
        auto_next: true,
        loop_list: true,
        shuffle_seed: Some(seed),
        ..ListOptions::default()
    };
    ListPlayer::new(library, MediaList::new(items, options).unwrap()).unwrap()
}

#[test]
fn shuffle_is_a_reproducible_permutation_reshuffled_per_cycle() {
    let mut first = shuffled_list(0x5eed);
    let mut second = shuffled_list(0x5eed);
    assert_eq!(first.order(), second.order());

    let mut sorted: Vec<_> = first.order().iter().map(|id| id.get()).collect();
    sorted.sort_unstable();
    assert_eq!(sorted, (0..8).collect::<Vec<u128>>());

    first.control(ListAction::First).unwrap();
    second.control(ListAction::First).unwrap();
    let a = run(&mut first, 0, 64);
    let b = run(&mut second, 0, 64);
    assert_eq!(a, b, "identical seeds produce identical output");
    assert_eq!(first.cycle(), 8);

    // Each 8-frame cycle plays every item exactly once in that cycle's order.
    let mut replay = shuffled_list(0x5eed);
    for cycle in a.chunks(8) {
        let expected: Vec<_> = replay
            .order()
            .iter()
            .map(|id| Some(u8::try_from(id.get()).unwrap()))
            .collect();
        assert_eq!(cycle, expected.as_slice());
        replay.control(ListAction::Play(replay.order()[7])).unwrap();
        replay.control(ListAction::Next).unwrap();
    }

    let orders: Vec<_> = (0..8)
        .map(|_| {
            let order = replay.order();
            replay.control(ListAction::Play(order[7])).unwrap();
            replay.control(ListAction::Next).unwrap();
            order
        })
        .collect();
    assert!(
        orders.windows(2).any(|pair| pair[0] != pair[1]),
        "cycles reshuffle"
    );
    let identity: Vec<_> = (0..8u128).map(ListItemId::new).collect();
    assert!(
        (0..4u64).any(|seed| shuffled_list(seed).order() != identity),
        "seeded order is not trivially the authored order"
    );
    assert_ne!(shuffled_list(1).order(), shuffled_list(2).order());
}

#[test]
fn list_construction_is_bounded_and_validated() {
    assert_eq!(
        MediaList::new(Vec::new(), ListOptions::default()),
        Err(ListError::Empty)
    );
    let too_many: Vec<_> = (0..=MAX_LIST_ITEMS)
        .map(|index| ListItem::new(ListItemId::new(index as u128), ClipId::new(1)))
        .collect();
    assert_eq!(
        MediaList::new(too_many, ListOptions::default()),
        Err(ListError::TooManyItems {
            count: MAX_LIST_ITEMS + 1,
            max: MAX_LIST_ITEMS,
        })
    );
    let at_limit: Vec<_> = (0..MAX_LIST_ITEMS)
        .map(|index| ListItem::new(ListItemId::new(index as u128), ClipId::new(1)))
        .collect();
    assert_eq!(
        MediaList::new(at_limit, ListOptions::default())
            .unwrap()
            .len(),
        MAX_LIST_ITEMS
    );
    assert_eq!(
        MediaList::new(
            vec![
                ListItem::new(A, ClipId::new(1)),
                ListItem::new(A, ClipId::new(2)),
            ],
            ListOptions::default()
        ),
        Err(ListError::DuplicateItem(A))
    );
    assert_eq!(
        MediaList::new(
            vec![ListItem::new(A, ClipId::new(1)).with_duration(0)],
            ListOptions::default()
        ),
        Err(ListError::ZeroDuration(A))
    );

    let missing = MediaList::new(
        vec![ListItem::new(A, ClipId::new(77))],
        ListOptions::default(),
    )
    .unwrap();
    assert_eq!(
        ListPlayer::new(library(), missing).unwrap_err(),
        ListError::Library(LibraryError::MissingClip(ClipId::new(77)))
    );
    let bad_marks = Marks::new(FrameIndex::new(0), FrameIndex::new(5)).unwrap();
    let invalid = MediaList::new(
        vec![ListItem::new(A, ClipId::new(1)).with_marks(bad_marks)],
        ListOptions::default(),
    )
    .unwrap();
    assert_eq!(
        ListPlayer::new(library(), invalid).unwrap_err(),
        ListError::InvalidMarks {
            item: A,
            source: PlaybackError::MarkOutOfRange {
                mark_out: FrameIndex::new(5),
                frame_count: 3,
            },
        }
    );
}

#[test]
fn pending_go_queue_is_bounded_and_history_can_be_pruned() {
    let mut player = player(abc(), ListOptions::default());
    for id in 0..MAX_PENDING_LIST_GO {
        let id = u64::try_from(id).unwrap();
        player
            .schedule_go(ListGo {
                id: GoId::new(u128::from(id)),
                coordinate: at(id),
                action: ListAction::First,
            })
            .unwrap();
    }
    assert_eq!(
        player.schedule_go(ListGo {
            id: GoId::new(u128::MAX),
            coordinate: at(5000),
            action: ListAction::First,
        }),
        Err(ListGoError::TooManyPending {
            max: MAX_PENDING_LIST_GO,
        })
    );
    let tick = player.tick(at(9), None).unwrap();
    let executed = tick
        .events
        .iter()
        .filter(|event| matches!(event, ListEvent::GoExecuted { .. }))
        .count();
    assert_eq!(executed, 10);
    assert_eq!(player.pending_go_count(), MAX_PENDING_LIST_GO - 10);

    assert_eq!(player.prune_go_history(at(4)), 5);
    assert_eq!(player.go_status(GoId::new(4)), None);
    assert_eq!(player.go_status(GoId::new(5)), Some(GoStatus::Executed));
    assert_eq!(
        player.go_status(GoId::new(500)),
        Some(GoStatus::Pending),
        "pending records are never pruned"
    );
    assert_eq!(player.prune_go_history(at(u64::MAX)), 5);
}

#[test]
fn playback_failure_is_typed_and_names_the_item() {
    let mut library = ClipLibrary::default();
    library
        .insert(
            FixtureClip::from_slots(ClipId::new(1), vec![Some(1u8), None])
                .unwrap()
                .into(),
        )
        .unwrap();
    let list = MediaList::new(vec![ListItem::new(A, ClipId::new(1))], auto_next()).unwrap();
    let mut player = ListPlayer::new(library, list).unwrap();
    player.control(ListAction::First).unwrap();
    assert_eq!(player.tick(at(0), None).unwrap().frame.unwrap().frame, 1);
    assert_eq!(
        player.tick(at(1), None),
        Err(ListTickError::Playback {
            item: A,
            source: PlaybackError::MissingFrame {
                clip: ClipId::new(1),
                frame: FrameIndex::new(1),
            },
        })
    );
    assert_eq!(
        player.tick(at(1), None),
        Err(ListTickError::CoordinateNotAdvanced {
            previous: at(1),
            requested: at(1),
        })
    );
}
