//! IN-014 in the engine: document inputs step page by page against the
//! page count the media plane reports, clamp at both ends, refuse while the
//! count is unknown, carry no undo entry, and replay deterministically.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, RejectionCode};
use fm_engine::{
    Engine, EngineCommand, EngineCommandOutcome, EngineEvent, EngineInputLifecycleEdit,
    EnginePagedStatus, ShowState,
};
use fm_model::{BatchImportRequest, GeneratorVideo, InputSourceSpec};
use fm_types::paged::{MAX_DOCUMENT_INPUTS, PagedNavigation};
use fm_types::{FrameRate, InputId};

const DECK: u128 = 3;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn deck_source() -> InputSourceSpec {
    InputSourceSpec::Document {
        asset_uri: "asset://deck.pdf".into(),
    }
}

/// Inputs 1 (Program), 2 (Preview), and document input 3.
fn engine() -> Engine {
    let mut show = ShowState::new(
        "paged",
        [(1, "Camera"), (2, "Bars"), (DECK, "Deck")]
            .into_iter()
            .map(|(value, name)| (input(value), name.to_owned()))
            .collect(),
        input(1),
        input(2),
    )
    .unwrap();
    for value in 1..=2 {
        show.set_input_source(
            input(value),
            InputSourceSpec::Generator {
                video: GeneratorVideo::Bars,
                tone_hz: None,
            },
        )
        .unwrap();
    }
    show.set_input_source(input(DECK), deck_source()).unwrap();
    Engine::new(
        show,
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn run(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    let outcome = engine
        .execute(
            CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command),
            0,
        )
        .unwrap();
    engine.tick().unwrap();
    outcome
}

fn step(
    engine: &mut Engine,
    key: &str,
    value: u128,
    navigation: PagedNavigation,
) -> EngineCommandOutcome {
    run(
        engine,
        key,
        EngineCommand::PageNavigate {
            input: input(value),
            navigation,
        },
    )
}

fn page(engine: &Engine) -> u32 {
    engine.show().paged_page(input(DECK)).unwrap()
}

#[test]
fn navigation_clamps_against_the_reported_count() {
    let mut engine = engine();
    assert_eq!(page(&engine), 0);
    assert!(engine.set_paged_count(input(DECK), Some(3)));
    assert_eq!(
        engine.paged_status(input(DECK)),
        Some(EnginePagedStatus {
            input: input(DECK),
            page: 0,
            count: Some(3),
        })
    );

    let previous = step(&mut engine, "previous", DECK, PagedNavigation::Previous);
    assert!(previous.receipt.accepted().is_some());
    assert_eq!(page(&engine), 0);

    let next = step(&mut engine, "next", DECK, PagedNavigation::Next);
    assert_eq!(
        next.events[0].payload,
        EngineEvent::PagedChanged {
            input: input(DECK),
            navigation: PagedNavigation::Next,
        }
    );
    assert_eq!(page(&engine), 1);
    step(&mut engine, "last", DECK, PagedNavigation::Last);
    assert_eq!(page(&engine), 2);
    step(&mut engine, "past-end", DECK, PagedNavigation::Next);
    assert_eq!(page(&engine), 2);
    step(&mut engine, "goto", DECK, PagedNavigation::Goto(1));
    assert_eq!(page(&engine), 1);
    step(&mut engine, "goto-far", DECK, PagedNavigation::Goto(99));
    assert_eq!(page(&engine), 2);
    step(&mut engine, "first", DECK, PagedNavigation::First);
    assert_eq!(page(&engine), 0);

    // Navigation is a live control: nothing to undo.
    assert_eq!(engine.undo_entries().count(), 0);

    // A shorter document pulls the page back inside it.
    step(&mut engine, "last-again", DECK, PagedNavigation::Last);
    engine.set_paged_count(input(DECK), Some(2));
    step(&mut engine, "next-short", DECK, PagedNavigation::Next);
    assert_eq!(page(&engine), 1);
}

#[test]
fn navigation_is_refused_while_the_count_is_unknown_or_the_input_is_not_paged() {
    let mut engine = engine();
    let unknown = step(&mut engine, "unknown", DECK, PagedNavigation::Next);
    let rejection = &unknown.receipt.rejected().unwrap().rejection;
    assert_eq!(rejection.code, RejectionCode::Unavailable);
    assert_eq!(rejection.fields[0].code, "pages_unknown");
    assert!(rejection.retryable);
    engine.set_paged_count(input(DECK), Some(0));
    assert!(
        step(&mut engine, "empty", DECK, PagedNavigation::Next)
            .receipt
            .rejected()
            .is_some()
    );

    let not_paged = step(&mut engine, "bars", 2, PagedNavigation::Next);
    let rejection = &not_paged.receipt.rejected().unwrap().rejection;
    assert_eq!(rejection.code, RejectionCode::InvalidCommand);
    assert_eq!(rejection.fields[0].code, "not_paged");

    let missing = step(&mut engine, "missing", 40, PagedNavigation::Next);
    assert_eq!(
        missing.receipt.rejected().unwrap().rejection.code,
        RejectionCode::NotFound
    );
    // A count for something that is not a document is ignored.
    assert!(!engine.set_paged_count(input(2), Some(4)));
}

#[test]
fn replaying_the_same_commands_lands_on_the_same_page() {
    let script = [
        PagedNavigation::Next,
        PagedNavigation::Next,
        PagedNavigation::Goto(7),
        PagedNavigation::Previous,
        PagedNavigation::Next,
        PagedNavigation::Next,
    ];
    let replay = || {
        let mut engine = engine();
        engine.set_paged_count(input(DECK), Some(5));
        let mut pages = Vec::new();
        for (index, navigation) in script.into_iter().enumerate() {
            step(&mut engine, &format!("step-{index}"), DECK, navigation);
            pages.push(page(&engine));
        }
        pages
    };
    let first = replay();
    assert_eq!(first, [1, 2, 4, 3, 4, 4]);
    assert_eq!(replay(), first);
}

#[test]
fn imports_and_replacements_create_documents_on_their_first_page() {
    let mut engine = engine();
    let outcome = run(
        &mut engine,
        "import",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::Import {
            items: vec![
                BatchImportRequest {
                    input: input(20),
                    name: None,
                    asset_uri: "asset://Keynote talk.pptx".into(),
                },
                BatchImportRequest {
                    input: input(21),
                    name: None,
                    asset_uri: "asset://clip.mov".into(),
                },
            ],
        }),
    );
    assert!(
        outcome.receipt.accepted().is_some(),
        "{:?}",
        outcome.receipt
    );
    assert_eq!(
        engine.show().input_source(input(20)),
        Some(&InputSourceSpec::Document {
            asset_uri: "asset://Keynote talk.pptx".into(),
        })
    );
    assert_eq!(engine.show().paged_page(input(20)), Some(0));
    assert!(engine.show().media_settings(input(20)).is_none());
    assert!(engine.show().media_settings(input(21)).is_some());
    assert_eq!(engine.show().paged_page(input(21)), None);

    // Duplicating a document copies its page.
    engine.set_paged_count(input(DECK), Some(4));
    step(&mut engine, "next", DECK, PagedNavigation::Goto(2));
    let duplicate = run(
        &mut engine,
        "duplicate",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::Duplicate {
            source: input(DECK),
            input: input(30),
            name: Some("Deck copy".into()),
        }),
    );
    assert!(
        duplicate.receipt.accepted().is_some(),
        "{:?}",
        duplicate.receipt
    );
    assert_eq!(engine.show().paged_page(input(30)), Some(2));

    // Replacing a source with a document starts it on its first page, and
    // replacing it away forgets the page.
    let replace = run(
        &mut engine,
        "replace",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::ReplaceSource {
            input: input(2),
            source: deck_source(),
        }),
    );
    assert!(
        replace.receipt.accepted().is_some(),
        "{:?}",
        replace.receipt
    );
    assert_eq!(engine.show().paged_page(input(2)), Some(0));
    let undo = run(
        &mut engine,
        "undo",
        EngineCommand::Undo {
            expected_edit: None,
        },
    );
    assert!(undo.receipt.accepted().is_some());
    assert_eq!(engine.show().paged_page(input(2)), None);
}

#[test]
fn the_document_count_is_bounded() {
    let mut engine = engine();
    let import = |engine: &mut Engine, key: &str, first: usize, count: usize| {
        let items = (first..first + count)
            .map(|index| BatchImportRequest {
                input: input(100 + index as u128),
                name: None,
                asset_uri: format!("asset://deck-{index}.pdf"),
            })
            .collect();
        run(
            engine,
            key,
            EngineCommand::InputLifecycle(EngineInputLifecycleEdit::Import { items }),
        )
    };
    // The fixture already holds one document.
    let fill = import(&mut engine, "fill", 0, MAX_DOCUMENT_INPUTS - 1);
    assert!(fill.receipt.accepted().is_some(), "{:?}", fill.receipt);
    assert_eq!(engine.show().document_inputs().count(), MAX_DOCUMENT_INPUTS);
    let over = import(&mut engine, "over", MAX_DOCUMENT_INPUTS, 1);
    let rejection = &over.receipt.rejected().unwrap().rejection;
    assert_eq!(rejection.fields[0].code, "too_many_document_inputs");
    let replace = run(
        &mut engine,
        "replace",
        EngineCommand::InputLifecycle(EngineInputLifecycleEdit::ReplaceSource {
            input: input(2),
            source: deck_source(),
        }),
    );
    assert!(replace.receipt.rejected().is_some());
}

/// A cue's page action steps a document like a direct page step: the page
/// moves with the GO, against the reported count.
#[test]
fn a_cue_page_action_steps_a_document() {
    let mut engine = engine();
    engine.set_paged_count(input(DECK), Some(5));
    let cue = |id, navigation| fm_model::Cue {
        id,
        name: format!("Cue {id}"),
        actions: vec![fm_model::CueAction::Page {
            input: input(DECK),
            navigation,
        }],
        auto_follow: false,
    };
    let set = run(
        &mut engine,
        "set",
        EngineCommand::SetCueList {
            list: 1,
            definition: Some(fm_engine::EngineCueListDefinition {
                name: "Slides".into(),
                cues: vec![
                    cue(1, PagedNavigation::Goto(3)),
                    cue(2, PagedNavigation::Next),
                    cue(3, PagedNavigation::Next),
                ],
            }),
        },
    );
    assert!(set.receipt.accepted().is_some(), "{:?}", set.receipt);
    let go = |engine: &mut Engine, key: &str| {
        let outcome = run(
            engine,
            key,
            EngineCommand::CueControl {
                list: 1,
                operation: fm_engine::EngineCueOperation::Go { cue: None },
            },
        );
        assert!(
            outcome.receipt.accepted().is_some(),
            "{:?}",
            outcome.receipt
        );
    };
    go(&mut engine, "go-1");
    assert_eq!(page(&engine), 3);
    go(&mut engine, "go-2");
    assert_eq!(page(&engine), 4);
    // The last page stays.
    go(&mut engine, "go-3");
    assert_eq!(page(&engine), 4);
}
