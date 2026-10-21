//! IN-006 in the engine: a media list input plays one item at a time through
//! its media transport. Creating and editing lists are undoable, padlocked
//! project edits; navigation and advances are live operations planned
//! against the pre-command show, so the same commands reach the same items
//! on the same frames. Shuffle order is a pure function of the seed and the
//! loop cycle, identical to `fm_playback`'s list player.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RejectionCode};
use fm_engine::fm_playback::{MediaEvent, MediaState, list_play_order};
use fm_engine::{
    EditLockScope, Engine, EngineCommand, EngineCommandKind, EngineCommandOutcome, EngineEvent,
    EngineMediaCommand, EngineMediaListCommand, EngineMediaListNavigation,
    EngineMediaListOperation, EngineMediaOperation, EngineMediaSource, ShowState,
};
use fm_model::{
    InputSourceSpec, MediaListItem, MediaListOptions, MediaListTransition, MediaListTransitionKind,
    media_list_play_order,
};
use fm_types::{FrameRate, InputId};

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

const LIST: u128 = 7;

fn rate() -> FrameRate {
    FrameRate::new(25, 1).unwrap()
}

fn item(id: u32, file: &str, marks: (u64, Option<u64>)) -> MediaListItem {
    MediaListItem {
        id,
        asset_uri: format!("asset://media/{file}"),
        title: None,
        mark_in: marks.0,
        mark_out: marks.1,
    }
}

/// Three five-frame items of three files.
fn items() -> Vec<MediaListItem> {
    vec![
        item(1, "a.mp4", (10, Some(15))),
        item(2, "b.mov", (20, Some(25))),
        item(3, "c.wav", (0, Some(5))),
    ]
}

fn engine() -> Engine {
    let show = ShowState::new(
        "lists",
        vec![(input(1), "Camera".into()), (input(2), "Slides".into())],
        input(1),
        input(2),
    )
    .unwrap();
    Engine::new(
        show,
        rate(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

/// Opens the source of every media input whose source is not open, as the
/// simulated daemon does after each command.
fn attach(engine: &mut Engine) {
    let inputs: Vec<InputId> = engine.show().media_inputs().keys().copied().collect();
    for media in inputs {
        if !engine.media_source_open(media) {
            let settings = *engine.show().media_settings(media).unwrap();
            engine
                .set_media_source(media, Some(EngineMediaSource::synthetic(&settings, rate())))
                .unwrap();
        }
    }
}

fn execute(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    let outcome = engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap();
    attach(engine);
    outcome
}

fn list_command(operation: EngineMediaListOperation) -> EngineCommand {
    EngineCommand::MediaList(EngineMediaListCommand {
        input: input(LIST),
        operation,
    })
}

fn accept(engine: &mut Engine, key: &str, operation: EngineMediaListOperation) {
    let outcome = execute(engine, key, list_command(operation));
    assert!(
        outcome.receipt.accepted().is_some(),
        "{key}: {:?}",
        outcome.receipt
    );
}

fn refused(engine: &mut Engine, key: &str, operation: EngineMediaListOperation) -> String {
    let outcome = execute(engine, key, list_command(operation));
    let rejection = &outcome.receipt.rejected().expect("refused").rejection;
    rejection.fields[0].code.clone()
}

fn create(engine: &mut Engine, options: MediaListOptions) {
    accept(
        engine,
        "create",
        EngineMediaListOperation::Create {
            name: "Walk-in".into(),
            items: items(),
            options,
        },
    );
}

fn navigate(engine: &mut Engine, key: &str, navigation: EngineMediaListNavigation) {
    accept(engine, key, EngineMediaListOperation::Navigate(navigation));
}

fn play(engine: &mut Engine, key: &str) {
    let outcome = execute(
        engine,
        key,
        EngineCommand::Media(EngineMediaCommand {
            input: input(LIST),
            operation: EngineMediaOperation::Play,
        }),
    );
    assert!(outcome.receipt.accepted().is_some());
}

fn active(engine: &Engine) -> u32 {
    engine.show().media_list(input(LIST)).unwrap().active
}

fn asset(engine: &Engine) -> String {
    match engine.show().input_source(input(LIST)) {
        Some(InputSourceSpec::Media { asset_uri }) => asset_uri.clone(),
        other => panic!("not a media source: {other:?}"),
    }
}

/// Ticks once; returns the list's (presented source frame, ended) on that
/// frame.
fn step(engine: &mut Engine) -> (u64, bool) {
    let frame = engine.tick().unwrap();
    let tick = frame
        .media
        .into_iter()
        .find(|media| media.input == input(LIST))
        .and_then(|media| media.tick)
        .expect("the list's transport is open");
    let ended = tick
        .events
        .iter()
        .any(|event| matches!(event, MediaEvent::Ended { .. }));
    (tick.frame.expect("a frame is presented").get(), ended)
}

/// Ticks until the active item's clip ends; returns how many frames it took.
fn run_to_end(engine: &mut Engine) -> u64 {
    for frames in 1..=1_000 {
        if step(engine).1 {
            return frames;
        }
    }
    panic!("the clip never ended");
}

#[test]
fn create_appends_a_media_input_on_its_first_item_and_undo_removes_it() {
    let mut engine = engine();
    let command = list_command(EngineMediaListOperation::Create {
        name: "Walk-in".into(),
        items: items(),
        options: MediaListOptions::default(),
    });
    assert_eq!(command.kind(), EngineCommandKind::ProjectEdit);
    let outcome = execute(&mut engine, "create", command);
    assert!(outcome.receipt.accepted().is_some());
    assert!(outcome.events.iter().any(|event| matches!(
        &event.payload,
        EngineEvent::MediaListChanged { input: changed, operation: EngineMediaListOperation::Create { .. }, .. }
            if *changed == input(LIST)
    )));
    assert_eq!(outcome.events.len(), 1);
    assert_eq!(engine.show().input_name(input(LIST)), Some("Walk-in"));
    assert_eq!(engine.show().inputs(), [input(1), input(2), input(LIST)]);
    assert_eq!(active(&engine), 1);
    assert_eq!(asset(&engine), "asset://media/a.mp4");
    let settings = engine.show().media_settings(input(LIST)).unwrap();
    assert_eq!((settings.trim_in, settings.trim_out), (10, Some(15)));
    let status = engine.media_status(input(LIST)).unwrap();
    assert_eq!((status.state, status.position), (MediaState::Stopped, 10));
    let list = engine.media_list_status(input(LIST)).unwrap();
    assert_eq!((list.order.as_slice(), list.position), (&[1, 2, 3][..], 0));

    let undo = execute(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert!(undo.receipt.accepted().is_some(), "{:?}", undo.receipt);
    assert_eq!(engine.show().inputs(), [input(1), input(2)]);
    assert!(engine.show().media_list(input(LIST)).is_none());
}

#[test]
fn create_refuses_stills_host_paths_and_duplicate_items() {
    let mut engine = engine();
    let create = |items| EngineMediaListOperation::Create {
        name: "List".into(),
        items,
        options: MediaListOptions::default(),
    };
    assert_eq!(
        refused(
            &mut engine,
            "still",
            create(vec![item(1, "a.png", (0, None))])
        ),
        "unsupported_type"
    );
    let mut host = item(1, "a.mp4", (0, None));
    host.asset_uri = "/home/show/a.mp4".into();
    assert_eq!(
        refused(&mut engine, "host", create(vec![host])),
        "invalid_uri"
    );
    assert_eq!(
        refused(
            &mut engine,
            "duplicate",
            create(vec![
                item(1, "a.mp4", (0, None)),
                item(1, "b.mp4", (0, None))
            ])
        ),
        "duplicate_item"
    );
    assert_eq!(
        refused(&mut engine, "empty", create(Vec::new())),
        "invalid_items"
    );
    assert_eq!(
        refused(
            &mut engine,
            "marks",
            create(vec![item(1, "a.mp4", (5, Some(5)))])
        ),
        "invalid_marks"
    );
}

#[test]
fn auto_next_plays_each_item_from_its_in_mark_and_holds_the_last_without_loop() {
    let mut engine = engine();
    create(
        &mut engine,
        MediaListOptions {
            auto_next: true,
            ..MediaListOptions::default()
        },
    );
    play(&mut engine, "play");
    // Frames 10..=14 play; the tick presenting 14 crosses the out mark.
    assert_eq!(step(&mut engine), (10, false));
    assert_eq!(run_to_end(&mut engine), 4);
    accept(
        &mut engine,
        "advance-1",
        EngineMediaListOperation::Advance { from: 1 },
    );
    assert_eq!(active(&engine), 2);
    assert_eq!(asset(&engine), "asset://media/b.mov");
    // The next file opened playing, at its in mark.
    assert_eq!(
        engine.media_status(input(LIST)).unwrap().state,
        MediaState::Playing
    );
    assert_eq!(step(&mut engine), (20, false));
    assert_eq!(run_to_end(&mut engine), 4);
    // A stale advance (the item that already ended) is refused.
    assert_eq!(
        refused(
            &mut engine,
            "stale",
            EngineMediaListOperation::Advance { from: 1 }
        ),
        "stale_advance"
    );
    accept(
        &mut engine,
        "advance-2",
        EngineMediaListOperation::Advance { from: 2 },
    );
    assert_eq!(active(&engine), 3);
    run_to_end(&mut engine);
    // The last item ended without loop or Auto First: nothing follows.
    assert_eq!(
        refused(
            &mut engine,
            "advance-3",
            EngineMediaListOperation::Advance { from: 3 }
        ),
        "end_of_list"
    );
    assert_eq!(active(&engine), 3);
}

#[test]
fn auto_first_cues_the_first_item_stopped_after_the_last() {
    let mut engine = engine();
    create(
        &mut engine,
        MediaListOptions {
            auto_next: true,
            auto_first: true,
            ..MediaListOptions::default()
        },
    );
    navigate(&mut engine, "last", EngineMediaListNavigation::Last);
    play(&mut engine, "play");
    run_to_end(&mut engine);
    accept(
        &mut engine,
        "advance",
        EngineMediaListOperation::Advance { from: 3 },
    );
    assert_eq!(active(&engine), 1);
    let status = engine.media_status(input(LIST)).unwrap();
    assert_eq!((status.state, status.position), (MediaState::Stopped, 10));
    assert_eq!(engine.show().media_list(input(LIST)).unwrap().cycle, 0);
}

#[test]
fn an_advance_needs_auto_next() {
    let mut engine = engine();
    create(&mut engine, MediaListOptions::default());
    assert_eq!(
        refused(
            &mut engine,
            "advance",
            EngineMediaListOperation::Advance { from: 1 }
        ),
        "auto_next_off"
    );
}

#[test]
fn loop_wraps_into_a_new_cycle_and_reshuffles_deterministically() {
    let seed = 0x5EED;
    let options = MediaListOptions {
        auto_next: true,
        loop_list: true,
        shuffle_seed: Some(seed),
        ..MediaListOptions::default()
    };
    let run = || {
        let mut engine = engine();
        create(&mut engine, options);
        play(&mut engine, "play");
        let mut played = vec![active(&engine)];
        for index in 0..6 {
            run_to_end(&mut engine);
            let from = active(&engine);
            accept(
                &mut engine,
                &format!("advance-{index}"),
                EngineMediaListOperation::Advance { from },
            );
            played.push(active(&engine));
        }
        (played, engine.show().media_list(input(LIST)).unwrap().cycle)
    };
    let (played, cycle) = run();
    // The same seed plays the same order every time.
    assert_eq!(run(), (played.clone(), cycle));
    assert_eq!(cycle, 2);
    // Each cycle is the seeded permutation for that cycle, and it is the
    // same permutation `fm_playback`'s list player uses.
    let ids = [1, 2, 3];
    for (cycle, chunk) in played.chunks(3).enumerate() {
        let cycle = u64::try_from(cycle).unwrap();
        let order = media_list_play_order(3, Some(seed), cycle);
        assert_eq!(order, list_play_order(3, Some(seed), cycle));
        let expected: Vec<u32> = order.iter().map(|index| ids[*index]).collect();
        assert_eq!(chunk, &expected[..chunk.len()]);
    }
}

#[test]
fn shuffle_orders_differ_by_seed_and_cover_every_item() {
    let a = media_list_play_order(20, Some(1), 0);
    let b = media_list_play_order(20, Some(2), 0);
    assert_ne!(a, b);
    assert_ne!(a, media_list_play_order(20, Some(1), 1));
    let mut sorted = a.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (0..20).collect::<Vec<_>>());
    assert_eq!(media_list_play_order(4, None, 9), [0, 1, 2, 3]);
}

#[test]
fn navigation_steps_the_play_order_and_keeps_playing_when_playing() {
    let mut engine = engine();
    create(&mut engine, MediaListOptions::default());
    let navigate_command = list_command(EngineMediaListOperation::Navigate(
        EngineMediaListNavigation::Next,
    ));
    assert_eq!(navigate_command.kind(), EngineCommandKind::LiveOperation);
    // Previous on the first item without loop stays.
    navigate(&mut engine, "previous", EngineMediaListNavigation::Previous);
    assert_eq!(active(&engine), 1);
    navigate(&mut engine, "next", EngineMediaListNavigation::Next);
    assert_eq!(active(&engine), 2);
    // Stopped: the item is cued, not played.
    assert_eq!(
        engine.media_status(input(LIST)).unwrap().state,
        MediaState::Stopped
    );
    play(&mut engine, "play");
    step(&mut engine);
    navigate(&mut engine, "goto", EngineMediaListNavigation::Goto(99));
    assert_eq!(active(&engine), 3);
    assert_eq!(
        engine.media_status(input(LIST)).unwrap().state,
        MediaState::Playing
    );
    // Next on the last item without loop stays; with an item id it jumps.
    navigate(&mut engine, "next-last", EngineMediaListNavigation::Next);
    assert_eq!(active(&engine), 3);
    navigate(&mut engine, "item", EngineMediaListNavigation::Item(1));
    assert_eq!(active(&engine), 1);
    assert_eq!(step(&mut engine).0, 10);
    assert_eq!(
        refused(
            &mut engine,
            "unknown",
            EngineMediaListOperation::Navigate(EngineMediaListNavigation::Item(42))
        ),
        "unknown_item"
    );
    // With loop, Previous on the first item wraps to the last.
    let looping = MediaListOptions {
        loop_list: true,
        ..MediaListOptions::default()
    };
    accept(
        &mut engine,
        "loop",
        EngineMediaListOperation::Set {
            items: items(),
            options: looping,
            active: None,
        },
    );
    navigate(&mut engine, "wrap", EngineMediaListNavigation::Previous);
    assert_eq!(active(&engine), 3);
    navigate(&mut engine, "wrap-next", EngineMediaListNavigation::Next);
    assert_eq!(active(&engine), 1);
    assert_eq!(engine.show().media_list(input(LIST)).unwrap().cycle, 1);
}

#[test]
fn an_item_of_the_open_file_is_recued_in_place_at_its_marks() {
    let mut engine = engine();
    accept(
        &mut engine,
        "create",
        EngineMediaListOperation::Create {
            name: "Cuts".into(),
            items: vec![
                item(1, "a.mp4", (10, Some(15))),
                item(2, "a.mp4", (100, Some(104))),
            ],
            options: MediaListOptions {
                auto_next: true,
                ..MediaListOptions::default()
            },
        },
    );
    play(&mut engine, "play");
    run_to_end(&mut engine);
    accept(
        &mut engine,
        "advance",
        EngineMediaListOperation::Advance { from: 1 },
    );
    // Same file: no reopen, the transport seeks to the second item's in
    // mark and plays it on the next frame.
    assert!(engine.media_source_open(input(LIST)));
    assert_eq!(step(&mut engine), (100, false));
    assert_eq!(run_to_end(&mut engine), 3);
}

#[test]
fn set_reorders_and_retrims_keeps_the_active_item_and_is_undoable() {
    let mut engine = engine();
    create(&mut engine, MediaListOptions::default());
    navigate(&mut engine, "next", EngineMediaListNavigation::Next);
    let mut reordered = items();
    reordered.reverse();
    reordered[1].mark_in = 21;
    reordered[1].title = Some("Interview".into());
    let set = list_command(EngineMediaListOperation::Set {
        items: reordered.clone(),
        options: MediaListOptions::default(),
        active: None,
    });
    assert_eq!(set.kind(), EngineCommandKind::ProjectEdit);
    assert!(
        execute(&mut engine, "set", set)
            .receipt
            .accepted()
            .is_some()
    );
    let list = engine.media_list_status(input(LIST)).unwrap();
    assert_eq!(list.order, [3, 2, 1]);
    assert_eq!((list.list.active, list.position), (2, 1));
    // The active item's new in mark trims its open transport.
    step(&mut engine);
    assert_eq!(
        engine.show().media_settings(input(LIST)).unwrap().trim_in,
        21
    );
    assert_eq!(engine.media_status(input(LIST)).unwrap().position, 21);
    assert_eq!(step(&mut engine).0, 21);

    let undo = execute(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert!(undo.receipt.accepted().is_some(), "{:?}", undo.receipt);
    let list = engine.show().media_list(input(LIST)).unwrap();
    assert_eq!(list.items, items());
    assert_eq!(list.active, 2);

    // Dropping the active item cues the first item in play order.
    accept(
        &mut engine,
        "drop",
        EngineMediaListOperation::Set {
            items: vec![
                item(3, "c.wav", (0, Some(5))),
                item(1, "a.mp4", (10, Some(15))),
            ],
            options: MediaListOptions::default(),
            active: None,
        },
    );
    assert_eq!(active(&engine), 3);
    assert_eq!(asset(&engine), "asset://media/c.wav");
}

#[test]
fn a_padlocked_list_refuses_edits_but_still_navigates() {
    let mut engine = engine();
    create(&mut engine, MediaListOptions::default());
    let lock = execute(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(LIST)),
            locked: true,
        },
    );
    assert!(lock.receipt.accepted().is_some());
    let outcome = execute(
        &mut engine,
        "set",
        list_command(EngineMediaListOperation::Set {
            items: items(),
            options: MediaListOptions::default(),
            active: None,
        }),
    );
    assert_eq!(
        outcome.receipt.rejected().unwrap().rejection.code,
        RejectionCode::Conflict
    );
    navigate(&mut engine, "next", EngineMediaListNavigation::Next);
    assert_eq!(active(&engine), 2);
}

#[test]
fn trimming_the_input_marks_its_active_item() {
    let mut engine = engine();
    create(&mut engine, MediaListOptions::default());
    let outcome = execute(
        &mut engine,
        "trim",
        EngineCommand::Media(EngineMediaCommand {
            input: input(LIST),
            operation: EngineMediaOperation::Trim {
                trim_in: 12,
                trim_out: Some(14),
            },
        }),
    );
    assert!(outcome.receipt.accepted().is_some());
    let list = engine.show().media_list(input(LIST)).unwrap();
    let marked = list.active_item().unwrap();
    assert_eq!((marked.mark_in, marked.mark_out), (12, Some(14)));
}

#[test]
fn a_list_input_refuses_a_source_replacement_and_a_plain_input_is_not_a_list() {
    let mut engine = engine();
    create(&mut engine, MediaListOptions::default());
    let outcome = execute(
        &mut engine,
        "replace",
        EngineCommand::InputLifecycle(fm_engine::EngineInputLifecycleEdit::ReplaceSource {
            input: input(LIST),
            source: InputSourceSpec::Media {
                asset_uri: "asset://media/other.mp4".into(),
            },
        }),
    );
    let rejection = &outcome.receipt.rejected().unwrap().rejection;
    assert_eq!(rejection.fields[0].code, "media_list_input");
    let outcome = execute(
        &mut engine,
        "plain",
        EngineCommand::MediaList(EngineMediaListCommand {
            input: input(1),
            operation: EngineMediaListOperation::Navigate(EngineMediaListNavigation::Next),
        }),
    );
    assert_eq!(
        outcome.receipt.rejected().unwrap().rejection.fields[0].code,
        "not_media_list"
    );
}

#[test]
fn a_snapshot_round_trip_keeps_the_list() {
    let mut engine = engine();
    create(
        &mut engine,
        MediaListOptions {
            shuffle_seed: Some(3),
            ..MediaListOptions::default()
        },
    );
    navigate(&mut engine, "last", EngineMediaListNavigation::Last);
    engine.tick().unwrap();
    let snapshot = engine.snapshot().unwrap();
    let restored = Engine::restore(snapshot).unwrap();
    assert_eq!(
        restored.media_list_status(input(LIST)),
        engine.media_list_status(input(LIST))
    );
}

#[test]
fn duplicating_a_list_input_respects_the_media_list_limit() {
    let mut engine = engine();
    for index in 0..fm_model::MAX_MEDIA_LISTS {
        let id = 100 + u128::try_from(index).unwrap();
        let outcome = execute(
            &mut engine,
            &format!("create-{id}"),
            EngineCommand::MediaList(EngineMediaListCommand {
                input: input(id),
                operation: EngineMediaListOperation::Create {
                    name: format!("List {id}"),
                    items: items(),
                    options: MediaListOptions::default(),
                },
            }),
        );
        assert!(
            outcome.receipt.accepted().is_some(),
            "{:?}",
            outcome.receipt
        );
    }
    let outcome = execute(
        &mut engine,
        "duplicate",
        EngineCommand::InputLifecycle(fm_engine::EngineInputLifecycleEdit::Duplicate {
            source: input(100),
            input: input(999),
            name: None,
        }),
    );
    let rejection = &outcome.receipt.rejected().expect("refused").rejection;
    assert_eq!(rejection.fields[0].code, "too_many_media_lists");
    assert_eq!(engine.show().media_lists().len(), fm_model::MAX_MEDIA_LISTS);
}

fn undo() -> EngineCommand {
    EngineCommand::Undo {
        expected_edit: None,
    }
}

fn redo() -> EngineCommand {
    EngineCommand::Redo {
        expected_edit: None,
    }
}

/// A list on item 2 whose `Set` then drops item 2, which cues item 1.
fn dropped_active() -> Engine {
    let mut engine = engine();
    create(&mut engine, MediaListOptions::default());
    navigate(&mut engine, "next", EngineMediaListNavigation::Next);
    assert_eq!(active(&engine), 2);
    accept(
        &mut engine,
        "drop",
        EngineMediaListOperation::Set {
            items: vec![
                item(1, "a.mp4", (10, Some(15))),
                item(3, "c.wav", (0, Some(5))),
            ],
            options: MediaListOptions::default(),
            active: None,
        },
    );
    assert_eq!(active(&engine), 1);
    assert_eq!(asset(&engine), "asset://media/a.mp4");
    engine
}

/// Undoing a `Set` that dropped the active item restores the items and the
/// item that was active, and its file; the edit the undo resolves to (what
/// the authority journals) carries that item, so a journal replay on a
/// restored engine reaches the same list.
#[test]
fn undoing_a_set_that_dropped_the_active_item_restores_it() {
    let mut engine = dropped_active();
    let journaled = engine
        .undo_entries()
        .last()
        .expect("the set is undoable")
        .inverse()
        .clone();
    assert!(
        matches!(
            &journaled,
            EngineCommand::MediaList(EngineMediaListCommand {
                operation: EngineMediaListOperation::Set {
                    active: Some(2),
                    ..
                },
                ..
            })
        ),
        "{journaled:?}"
    );
    let outcome = execute(&mut engine, "undo", undo());
    assert!(
        outcome.receipt.accepted().is_some(),
        "{:?}",
        outcome.receipt
    );
    assert_eq!(
        engine.show().media_list(input(LIST)).unwrap().items,
        items()
    );
    assert_eq!(active(&engine), 2);
    assert_eq!(asset(&engine), "asset://media/b.mov");
    assert_eq!(
        engine.show().media_settings(input(LIST)).unwrap().trim_in,
        20
    );
    // Redo and undo again agree.
    assert!(
        execute(&mut engine, "redo", redo())
            .receipt
            .accepted()
            .is_some()
    );
    assert_eq!(active(&engine), 1);
    assert!(
        execute(&mut engine, "undo-again", undo())
            .receipt
            .accepted()
            .is_some()
    );
    assert_eq!(active(&engine), 2);

    let mut replayed = dropped_active();
    let outcome = execute(&mut replayed, "undo", journaled);
    assert!(
        outcome.receipt.accepted().is_some(),
        "{:?}",
        outcome.receipt
    );
    assert_eq!(
        replayed.show().media_list(input(LIST)),
        engine.show().media_list(input(LIST))
    );
    assert_eq!(asset(&replayed), "asset://media/b.mov");
}

/// A `Set` naming an `active` item activates it; one naming an item the
/// new items lack is refused.
#[test]
fn a_set_may_name_the_active_item() {
    let mut engine = engine();
    create(&mut engine, MediaListOptions::default());
    accept(
        &mut engine,
        "set-active",
        EngineMediaListOperation::Set {
            items: items(),
            options: MediaListOptions::default(),
            active: Some(3),
        },
    );
    assert_eq!(active(&engine), 3);
    assert_eq!(asset(&engine), "asset://media/c.wav");
    assert_eq!(
        refused(
            &mut engine,
            "set-unknown",
            EngineMediaListOperation::Set {
                items: items(),
                options: MediaListOptions::default(),
                active: Some(9),
            },
        ),
        "unknown_item"
    );
    assert_eq!(active(&engine), 3);
}

/// Navigating after an edit (by hand, or Auto Next while it plays) does not
/// block undoing the edit, and the undo leaves the item navigated to active
/// when the restored items keep it; redo after another navigation likewise.
#[test]
fn navigating_after_a_set_keeps_it_undoable_and_keeps_the_navigated_item() {
    let mut engine = engine();
    create(&mut engine, MediaListOptions::default());
    let retrimmed = vec![
        item(1, "a.mp4", (11, Some(15))),
        item(2, "b.mov", (20, Some(25))),
        item(3, "c.wav", (0, Some(5))),
    ];
    accept(
        &mut engine,
        "retrim",
        EngineMediaListOperation::Set {
            items: retrimmed.clone(),
            options: MediaListOptions::default(),
            active: None,
        },
    );
    assert_eq!(active(&engine), 1);
    navigate(&mut engine, "next", EngineMediaListNavigation::Next);
    assert_eq!(active(&engine), 2);
    let outcome = execute(&mut engine, "undo", undo());
    assert!(
        outcome.receipt.accepted().is_some(),
        "{:?}",
        outcome.receipt
    );
    assert_eq!(
        engine.show().media_list(input(LIST)).unwrap().items,
        items()
    );
    assert_eq!(active(&engine), 2, "the undo keeps the navigated item");
    assert_eq!(asset(&engine), "asset://media/b.mov");
    navigate(&mut engine, "next-again", EngineMediaListNavigation::Next);
    assert_eq!(active(&engine), 3);
    let outcome = execute(&mut engine, "redo", redo());
    assert!(
        outcome.receipt.accepted().is_some(),
        "{:?}",
        outcome.receipt
    );
    assert_eq!(
        engine.show().media_list(input(LIST)).unwrap().items,
        retrimmed
    );
    assert_eq!(active(&engine), 3, "the redo keeps the navigated item");
    assert!(
        execute(&mut engine, "undo-again", undo())
            .receipt
            .accepted()
            .is_some()
    );
    assert_eq!(active(&engine), 3);
}

/// Undoing a Set while the list plays an item the undo drops keeps playing
/// the current file when the restored list still has an item of it: that
/// item becomes active in place (its marks apply), the transport keeps
/// playing, and the file is not reopened.
#[test]
fn an_undo_dropping_the_playing_item_keeps_playing_its_file() {
    let mut engine = engine();
    create(&mut engine, MediaListOptions::default());
    let mut extended = items();
    extended.push(item(4, "c.wav", (2, Some(5))));
    accept(
        &mut engine,
        "second-cut",
        EngineMediaListOperation::Set {
            items: extended,
            options: MediaListOptions::default(),
            active: None,
        },
    );
    navigate(&mut engine, "to-cut", EngineMediaListNavigation::Item(4));
    play(&mut engine, "play");
    step(&mut engine);
    assert_eq!(asset(&engine), "asset://media/c.wav");
    let outcome = execute(&mut engine, "undo", undo());
    assert!(
        outcome.receipt.accepted().is_some(),
        "{:?}",
        outcome.receipt
    );
    assert_eq!(
        engine.show().media_list(input(LIST)).unwrap().items,
        items()
    );
    assert_eq!(active(&engine), 3, "the restored item of the same file");
    assert_eq!(asset(&engine), "asset://media/c.wav");
    assert!(engine.media_source_open(input(LIST)));
    step(&mut engine);
    assert_eq!(
        engine.media_status(input(LIST)).unwrap().state,
        MediaState::Playing
    );
}

/// Undoing a Set while the list plays an item the undo drops, with no item
/// of its file left, restores the item the edit left (not the first item).
#[test]
fn an_undo_dropping_the_playing_file_restores_the_edited_item() {
    let mut engine = engine();
    create(&mut engine, MediaListOptions::default());
    navigate(&mut engine, "to-b", EngineMediaListNavigation::Item(2));
    let mut extended = items();
    extended.push(item(4, "d.mp4", (0, Some(5))));
    accept(
        &mut engine,
        "add-d",
        EngineMediaListOperation::Set {
            items: extended,
            options: MediaListOptions::default(),
            active: None,
        },
    );
    navigate(&mut engine, "to-d", EngineMediaListNavigation::Item(4));
    play(&mut engine, "play");
    step(&mut engine);
    let outcome = execute(&mut engine, "undo", undo());
    assert!(
        outcome.receipt.accepted().is_some(),
        "{:?}",
        outcome.receipt
    );
    assert_eq!(active(&engine), 2);
    assert_eq!(asset(&engine), "asset://media/b.mov");
}

/// Ticks once; returns the list's presented source frame and, during a
/// transition, the outgoing item's presented frame, step, and incoming
/// weight.
fn blend_step(engine: &mut Engine) -> (u64, Option<(u64, u32, u16)>) {
    let frame = engine.tick().unwrap();
    let media = frame
        .media
        .into_iter()
        .find(|media| media.input == input(LIST))
        .expect("the list reports every frame");
    let presented = media
        .tick
        .and_then(|tick| tick.frame)
        .expect("a frame is presented")
        .get();
    let outgoing = media.outgoing.map(|outgoing| {
        assert_eq!(outgoing.frames, 4);
        (
            outgoing
                .tick
                .frame
                .expect("the outgoing item presents")
                .get(),
            outgoing.step,
            outgoing.mix_basis_points,
        )
    });
    (presented, outgoing)
}

fn faded(kind: MediaListTransitionKind) -> MediaListOptions {
    MediaListOptions {
        auto_next: true,
        transition: MediaListTransition {
            kind,
            duration_frames: 4,
        },
        ..MediaListOptions::default()
    }
}

/// Two ten-frame items of two files.
fn long_items() -> Vec<MediaListItem> {
    vec![
        item(1, "a.mp4", (10, Some(20))),
        item(2, "b.mov", (20, Some(30))),
    ]
}

fn create_long(engine: &mut Engine, options: MediaListOptions) {
    accept(
        engine,
        "create",
        EngineMediaListOperation::Create {
            name: "Walk-in".into(),
            items: long_items(),
            options,
        },
    );
}

#[test]
fn a_transition_weights_the_incoming_item_by_its_step() {
    let fade = MediaListTransition {
        kind: MediaListTransitionKind::Fade,
        duration_frames: 4,
    };
    assert_eq!(
        (1..=5)
            .map(|step| fade.mix_basis_points(step))
            .collect::<Vec<_>>(),
        [2_000, 4_000, 6_000, 8_000, 10_000]
    );
    // A cut keeps its length but composites nothing.
    let cut = MediaListTransition {
        kind: MediaListTransitionKind::Cut,
        duration_frames: 4,
    };
    assert_eq!((cut.blend_frames(), cut.mix_basis_points(1)), (0, 10_000));
    assert_eq!(
        MediaListOptions::default().transition,
        MediaListTransition::CUT
    );
}

#[test]
fn a_playing_step_to_another_file_plays_the_outgoing_item_under_the_incoming_one() {
    let mut engine = engine();
    create_long(&mut engine, faded(MediaListTransitionKind::Fade));
    play(&mut engine, "play");
    for expected in 10..=12 {
        assert_eq!(blend_step(&mut engine), (expected, None));
    }
    navigate(&mut engine, "next", EngineMediaListNavigation::Next);
    assert_eq!(asset(&engine), "asset://media/b.mov");
    // The outgoing item keeps playing from where it was for four frames,
    // its weight falling as the incoming one's rises; then it is gone.
    assert!(engine.media_advancing());
    assert_eq!(blend_step(&mut engine), (20, Some((13, 1, 2_000))));
    assert_eq!(blend_step(&mut engine), (21, Some((14, 2, 4_000))));
    assert_eq!(blend_step(&mut engine), (22, Some((15, 3, 6_000))));
    assert_eq!(blend_step(&mut engine), (23, Some((16, 4, 8_000))));
    assert_eq!(blend_step(&mut engine), (24, None));
}

#[test]
fn an_outgoing_item_holds_its_last_frame_when_it_ends_first() {
    let mut engine = engine();
    create_long(&mut engine, faded(MediaListTransitionKind::Dip));
    play(&mut engine, "play");
    for _ in 10..=17 {
        blend_step(&mut engine);
    }
    navigate(&mut engine, "next", EngineMediaListNavigation::Next);
    assert_eq!(blend_step(&mut engine), (20, Some((18, 1, 2_000))));
    assert_eq!(blend_step(&mut engine), (21, Some((19, 2, 4_000))));
    // Past its out mark the outgoing item holds its last frame.
    assert_eq!(blend_step(&mut engine), (22, Some((19, 3, 6_000))));
    assert_eq!(blend_step(&mut engine), (23, Some((19, 4, 8_000))));
    assert_eq!(blend_step(&mut engine), (24, None));
}

#[test]
fn a_step_to_an_item_of_the_open_file_blends_from_the_open_transport() {
    let mut engine = engine();
    accept(
        &mut engine,
        "create",
        EngineMediaListOperation::Create {
            name: "Cuts".into(),
            items: vec![
                item(1, "a.mp4", (10, Some(20))),
                item(2, "a.mp4", (100, Some(110))),
            ],
            options: faded(MediaListTransitionKind::Wipe),
        },
    );
    play(&mut engine, "play");
    for expected in 10..=12 {
        assert_eq!(blend_step(&mut engine), (expected, None));
    }
    navigate(&mut engine, "next", EngineMediaListNavigation::Next);
    assert!(engine.media_source_open(input(LIST)));
    assert_eq!(blend_step(&mut engine), (100, Some((13, 1, 2_000))));
    assert_eq!(blend_step(&mut engine), (101, Some((14, 2, 4_000))));
    assert_eq!(blend_step(&mut engine), (102, Some((15, 3, 6_000))));
    assert_eq!(blend_step(&mut engine), (103, Some((16, 4, 8_000))));
    assert_eq!(blend_step(&mut engine), (104, None));
}

#[test]
fn a_cut_or_a_stopped_list_steps_without_a_transition() {
    // A cut never composites.
    let mut engine = engine();
    create_long(
        &mut engine,
        MediaListOptions {
            auto_next: true,
            ..MediaListOptions::default()
        },
    );
    play(&mut engine, "play");
    blend_step(&mut engine);
    navigate(&mut engine, "next", EngineMediaListNavigation::Next);
    assert_eq!(blend_step(&mut engine), (20, None));

    // A stopped list cues the next item without blending.
    let mut engine = self::engine();
    create_long(&mut engine, faded(MediaListTransitionKind::Fade));
    navigate(&mut engine, "next", EngineMediaListNavigation::Next);
    let frame = engine.tick().unwrap();
    assert!(
        frame
            .media
            .iter()
            .filter(|media| media.input == input(LIST))
            .all(|media| media.outgoing.is_none())
    );
}

#[test]
fn the_transition_is_validated_and_undoable() {
    let mut engine = engine();
    create(&mut engine, MediaListOptions::default());
    for frames in [0, fm_model::MAX_MEDIA_LIST_TRANSITION_FRAMES + 1] {
        let mut options = faded(MediaListTransitionKind::Fade);
        options.transition.duration_frames = frames;
        assert_eq!(
            refused(
                &mut engine,
                &format!("bad-{frames}"),
                EngineMediaListOperation::Set {
                    items: items(),
                    options,
                    active: None,
                },
            ),
            "invalid_transition"
        );
    }
    accept(
        &mut engine,
        "fade",
        EngineMediaListOperation::Set {
            items: items(),
            options: faded(MediaListTransitionKind::Fade),
            active: None,
        },
    );
    let options = |engine: &Engine| engine.show().media_list(input(LIST)).unwrap().options;
    assert_eq!(options(&engine), faded(MediaListTransitionKind::Fade));
    assert!(
        execute(&mut engine, "undo", undo())
            .receipt
            .accepted()
            .is_some()
    );
    assert_eq!(options(&engine).transition, MediaListTransition::CUT);
    assert!(
        execute(&mut engine, "redo", redo())
            .receipt
            .accepted()
            .is_some()
    );
    assert_eq!(options(&engine), faded(MediaListTransitionKind::Fade));
}
