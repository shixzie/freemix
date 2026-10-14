//! Title inputs: live field state, title take/off-air, and their refusals.

use core::num::NonZeroU128;

use fm_clock::ClockDomainId;
use fm_command::{CommandEnvelope, IdempotencyKey, Rejection, RejectionCode};
use fm_engine::{
    EditLockScope, Engine, EngineCommand, EngineCommandKind, EngineCommandOutcome, EngineEvent,
    EngineTitle, ShowError, ShowState, TitleAirRequirement, TitleRejectionKind,
};
use fm_switcher::{OverlayChannelId, OverlayTransitionKind};
use fm_titles::{
    Alignment, Bounds, Color, Element, ElementId, ElementKind, FieldDefinition, FieldId,
    FieldValue, ImageSource, MAX_TITLE_CONTENT_BYTES, MAX_TITLE_TEXT_CHARS, Style, TemplateId,
    TitleFieldSet, TitleTemplate,
};
use fm_types::{FrameRate, InputId};

const PROGRAM: u128 = 1;
const PREVIEW: u128 = 2;
const TITLE: u128 = 3;
const OTHER_TITLE: u128 = 4;
const PLAIN: u128 = 5;

const NAME: u128 = 1;
const FILL: u128 = 2;
const LOGO: u128 = 3;
const SCORE: u128 = 4;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn field(value: u128) -> FieldId {
    FieldId::new(NonZeroU128::new(value).unwrap())
}

fn channel(number: u8) -> OverlayChannelId {
    OverlayChannelId::new(number).unwrap()
}

fn definition(id: u128, default: FieldValue) -> FieldDefinition {
    FieldDefinition {
        id: field(id),
        name: format!("field-{id}"),
        default,
    }
}

/// A lower third drawn as one rectangle whose fill is a color field, so no
/// font is needed anywhere.
fn template() -> TitleTemplate {
    TitleTemplate {
        id: TemplateId::new(NonZeroU128::new(9).unwrap()),
        name: "Lower third".into(),
        width: 64,
        height: 16,
        background: Color::TRANSPARENT,
        fields: vec![
            definition(NAME, FieldValue::Text(String::new())),
            definition(FILL, FieldValue::Color(Color::BLACK)),
            definition(
                LOGO,
                FieldValue::Image(ImageSource::new("asset://logo.png")),
            ),
            definition(SCORE, FieldValue::Number(0)),
        ],
        elements: vec![Element {
            id: ElementId::new(NonZeroU128::new(1).unwrap()),
            name: "bar".into(),
            bounds: Bounds {
                x: 0,
                y: 0,
                width: 64,
                height: 16,
            },
            z_index: 0,
            visible: true,
            style: Style {
                fill: Color::BLACK,
                background: None,
                opacity: 255,
                font: None,
                alignment: Alignment::default(),
            },
            color_field: Some(field(FILL)),
            kind: ElementKind::Rectangle,
        }],
        animation: fm_titles::TitleAnimation::default(),
    }
}

fn title() -> EngineTitle {
    EngineTitle::new(
        "Lower third",
        TitleFieldSet::from_template(&template()).unwrap(),
    )
}

fn show() -> ShowState {
    let mut show = ShowState::new(
        "title show",
        (1..=PLAIN)
            .map(|value| (input(value), format!("Input {value}")))
            .collect(),
        input(PROGRAM),
        input(PREVIEW),
    )
    .unwrap();
    show.set_title(input(TITLE), title()).unwrap();
    show.set_title(input(OTHER_TITLE), title()).unwrap();
    show
}

fn engine_with(show: ShowState) -> Engine {
    Engine::new(
        show,
        FrameRate::new(25, 1).unwrap(),
        ClockDomainId::new(NonZeroU128::new(100).unwrap()),
    )
}

fn engine() -> Engine {
    engine_with(show())
}

fn envelope(key: &str, command: EngineCommand) -> CommandEnvelope<EngineCommand> {
    CommandEnvelope::new(format!("command-{key}"), IdempotencyKey::new(key), command)
}

fn execute(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    engine.execute(envelope(key, command), 0).unwrap()
}

/// Executes one command and realizes it with a single tick.
fn run(engine: &mut Engine, key: &str, command: EngineCommand) -> EngineCommandOutcome {
    let outcome = execute(engine, key, command);
    engine.tick().unwrap();
    outcome
}

fn ticks(engine: &mut Engine, count: usize) {
    for _ in 0..count {
        engine.tick().unwrap();
    }
}

fn text(value: &str) -> FieldValue {
    FieldValue::Text(value.to_owned())
}

fn set(
    base_revision: u64,
    require: TitleAirRequirement,
    fields: Vec<(FieldId, FieldValue)>,
) -> EngineCommand {
    set_on(TITLE, base_revision, require, fields)
}

fn set_on(
    title: u128,
    base_revision: u64,
    require: TitleAirRequirement,
    fields: Vec<(FieldId, FieldValue)>,
) -> EngineCommand {
    EngineCommand::SetTitleFields {
        input: input(title),
        base_revision,
        require,
        fields,
    }
}

fn save(base_revision: u64, fields: Vec<(FieldId, FieldValue)>) -> EngineCommand {
    set(base_revision, TitleAirRequirement::OffAir, fields)
}

fn update(base_revision: u64, fields: Vec<(FieldId, FieldValue)>) -> EngineCommand {
    set(base_revision, TitleAirRequirement::OnAir, fields)
}

fn take(on: u8, base_revision: u64, fields: Vec<(FieldId, FieldValue)>) -> EngineCommand {
    EngineCommand::TakeTitle {
        input: input(TITLE),
        channel: channel(on),
        base_revision,
        fields,
    }
}

fn off_air(title: u128) -> EngineCommand {
    EngineCommand::TitleOffAir {
        input: input(title),
    }
}

fn rejection(outcome: &EngineCommandOutcome) -> &Rejection {
    &outcome
        .receipt
        .rejected()
        .expect("command should be rejected")
        .rejection
}

/// Asserts a title refusal with its stable code, receipt code, and field.
fn assert_refused(
    outcome: &EngineCommandOutcome,
    kind: TitleRejectionKind,
    code: RejectionCode,
    field_name: &str,
) {
    let rejection = rejection(outcome);
    assert_eq!(
        TitleRejectionKind::of(rejection),
        Some(kind),
        "{rejection:?}"
    );
    assert_eq!(rejection.fields[0].code, kind.code());
    assert_eq!(rejection.code, code, "{rejection:?}");
    assert_eq!(rejection.fields[0].field, field_name, "{rejection:?}");
}

fn assert_accepted(outcome: &EngineCommandOutcome) {
    assert!(
        outcome.receipt.accepted().is_some(),
        "expected acceptance, got {:?}",
        outcome.receipt
    );
}

fn payloads(outcome: &EngineCommandOutcome) -> Vec<EngineEvent> {
    outcome
        .events
        .iter()
        .map(|event| event.payload.clone())
        .collect()
}

fn fields(engine: &Engine, title: u128) -> &TitleFieldSet {
    &engine.show().title(input(title)).unwrap().fields
}

fn revision(engine: &Engine) -> u64 {
    fields(engine, TITLE).revision()
}

fn desired_shows(engine: &Engine, on: u8) -> bool {
    let overlay = engine.show().desired_switcher().overlay(channel(on));
    overlay.is_active() && overlay.source() == Some(input(TITLE))
}

fn realized_shows(engine: &Engine, on: u8) -> bool {
    let overlay = engine.realized_switcher().overlay(channel(on));
    overlay.is_active() && overlay.source() == Some(input(TITLE))
}

fn fade_overlay(engine: &mut Engine, key: &str, on: u8, duration_frames: u32) {
    assert_accepted(&run(
        engine,
        key,
        EngineCommand::ConfigureOverlayTransition {
            channel: channel(on),
            transition: OverlayTransitionKind::Fade,
            duration_frames,
        },
    ));
}

#[test]
fn show_state_hosts_titles_only_for_its_inputs_and_drops_them_with_the_input() {
    let mut show = show();
    assert_eq!(
        show.titles().keys().copied().collect::<Vec<_>>(),
        vec![input(TITLE), input(OTHER_TITLE)]
    );
    assert_eq!(show.title(input(TITLE)).unwrap().name, "Lower third");
    assert!(show.title(input(PLAIN)).is_none());
    assert_eq!(
        show.set_title(input(99), title()),
        Err(ShowError::UnknownInput(input(99)))
    );

    let mut engine = engine_with(show.clone());
    assert_accepted(&run(
        &mut engine,
        "remove",
        EngineCommand::RemoveInput {
            input: input(OTHER_TITLE),
        },
    ));
    assert!(engine.show().title(input(OTHER_TITLE)).is_none());
    assert!(engine.show().title(input(TITLE)).is_some());

    // Replacing a title resets it to the supplied field set.
    let mut restored = TitleFieldSet::from_template(&template()).unwrap();
    restored
        .update(0, &[(field(SCORE), FieldValue::Number(3))])
        .unwrap();
    show.set_title(input(TITLE), EngineTitle::new("Score", restored.clone()))
        .unwrap();
    assert_eq!(show.title(input(TITLE)).unwrap().fields, restored);
}

#[test]
fn title_commands_are_live_operations_and_never_enter_history() {
    for command in [save(0, Vec::new()), take(1, 0, Vec::new()), off_air(TITLE)] {
        assert_eq!(command.kind(), EngineCommandKind::LiveOperation);
    }

    let mut engine = engine();
    assert_accepted(&run(
        &mut engine,
        "lock",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Project,
            locked: true,
        },
    ));
    assert_accepted(&run(
        &mut engine,
        "lock-title",
        EngineCommand::SetEditLock {
            scope: EditLockScope::Input(input(TITLE)),
            locked: true,
        },
    ));
    // Padlocks guard layout, not content: every title command still runs.
    assert_accepted(&run(
        &mut engine,
        "save",
        save(0, vec![(field(NAME), text("Ada"))]),
    ));
    assert_accepted(&run(&mut engine, "take", take(1, 1, Vec::new())));
    assert_accepted(&run(
        &mut engine,
        "update",
        update(1, vec![(field(SCORE), FieldValue::Number(1))]),
    ));
    assert_accepted(&run(&mut engine, "off", off_air(TITLE)));
    assert_eq!(engine.undo_entries().len(), 0);
    assert_eq!(engine.redo_entries().len(), 0);
    assert_eq!(revision(&engine), 2);
}

#[test]
fn saving_fields_advances_the_revision_once_and_reports_it() {
    let mut engine = engine();
    let outcome = run(
        &mut engine,
        "save",
        save(
            0,
            vec![
                (field(NAME), text("Grace Hopper")),
                (field(FILL), FieldValue::Color(Color::new(10, 20, 30, 255))),
            ],
        ),
    );
    assert_accepted(&outcome);
    assert_eq!(
        payloads(&outcome),
        vec![EngineEvent::TitleFieldsChanged {
            input: input(TITLE),
            revision: 1,
        }]
    );
    assert_eq!(revision(&engine), 1);
    assert_eq!(
        fields(&engine, TITLE).value(field(NAME)),
        Some(&text("Grace Hopper"))
    );
    assert_eq!(
        fields(&engine, TITLE).value(field(FILL)),
        Some(&FieldValue::Color(Color::new(10, 20, 30, 255)))
    );
    // The other title is untouched.
    assert_eq!(fields(&engine, OTHER_TITLE).revision(), 0);

    // An empty edit is accepted with one event but keeps the revision.
    let empty = run(&mut engine, "empty", save(1, Vec::new()));
    assert_accepted(&empty);
    assert_eq!(
        payloads(&empty),
        vec![EngineEvent::TitleFieldsChanged {
            input: input(TITLE),
            revision: 1,
        }]
    );
    assert_eq!(revision(&engine), 1);
    // Every accepted command keeps the counters in lockstep.
    assert_eq!(engine.revision().get(), engine.event_sequence().get());
    assert_eq!(engine.revision().get(), engine.runtime_generation().get());
}

#[test]
fn field_refusals_carry_stable_codes_and_change_nothing() {
    let mut engine = engine();
    assert_accepted(&run(
        &mut engine,
        "seed",
        save(0, vec![(field(NAME), text("Ada"))]),
    ));
    let before = engine.show().clone();

    let cases = [
        (
            save(0, vec![(field(NAME), text("stale"))]),
            TitleRejectionKind::RevisionConflict,
            RejectionCode::Conflict,
            "base_revision",
        ),
        (
            save(1, vec![(field(77), text("x"))]),
            TitleRejectionKind::UnknownField,
            RejectionCode::InvalidCommand,
            "fields",
        ),
        (
            save(1, vec![(field(NAME), text("a")), (field(NAME), text("b"))]),
            TitleRejectionKind::DuplicateField,
            RejectionCode::InvalidCommand,
            "fields",
        ),
        (
            save(1, vec![(field(FILL), text("red"))]),
            TitleRejectionKind::FieldTypeMismatch,
            RejectionCode::InvalidCommand,
            "fields",
        ),
        (
            save(
                1,
                vec![(field(LOGO), FieldValue::Image(ImageSource::new("  ")))],
            ),
            TitleRejectionKind::InvalidFieldValue,
            RejectionCode::InvalidCommand,
            "fields",
        ),
        (
            save(
                1,
                vec![(field(NAME), text(&"x".repeat(MAX_TITLE_TEXT_CHARS + 1)))],
            ),
            TitleRejectionKind::ContentTooLarge,
            RejectionCode::InvalidCommand,
            "fields",
        ),
        (
            set_on(PLAIN, 0, TitleAirRequirement::OffAir, Vec::new()),
            TitleRejectionKind::UnknownTitle,
            RejectionCode::NotFound,
            "input",
        ),
        (
            set_on(99, 0, TitleAirRequirement::OffAir, Vec::new()),
            TitleRejectionKind::UnknownTitle,
            RejectionCode::NotFound,
            "input",
        ),
    ];
    for (index, (command, kind, code, field_name)) in cases.into_iter().enumerate() {
        let outcome = run(&mut engine, &format!("refused-{index}"), command);
        assert_refused(&outcome, kind, code, field_name);
        assert_eq!(outcome.events.len(), 0);
        assert_eq!(engine.show(), &before, "case {index} changed the show");
    }

    // The total content bound counts text and image-asset bytes together.
    let filler = "x".repeat(MAX_TITLE_TEXT_CHARS);
    assert_accepted(&run(
        &mut engine,
        "fill-name",
        save(1, vec![(field(NAME), text(&filler))]),
    ));
    // Replacing the logo frees its bytes; one byte over the bound is refused.
    let asset = format!(
        "asset://{}",
        "y".repeat(MAX_TITLE_CONTENT_BYTES - filler.len() + 1 - "asset://".len())
    );
    let too_large = run(
        &mut engine,
        "too-large",
        save(
            2,
            vec![(field(LOGO), FieldValue::Image(ImageSource::new(asset)))],
        ),
    );
    assert_refused(
        &too_large,
        TitleRejectionKind::ContentTooLarge,
        RejectionCode::InvalidCommand,
        "fields",
    );
    assert_eq!(revision(&engine), 2);
}

#[test]
fn an_exhausted_revision_is_refused() {
    let mut show = show();
    let values = template()
        .fields
        .iter()
        .map(|definition| (definition.id, definition.default.clone()))
        .collect::<Vec<_>>();
    show.set_title(
        input(TITLE),
        EngineTitle::new(
            "Lower third",
            TitleFieldSet::restore(&template(), u64::MAX, values).unwrap(),
        ),
    )
    .unwrap();
    let mut engine = engine_with(show);
    assert_refused(
        &run(
            &mut engine,
            "exhausted",
            save(u64::MAX, vec![(field(SCORE), FieldValue::Number(1))]),
        ),
        TitleRejectionKind::RevisionExhausted,
        RejectionCode::ResourceExhausted,
        "base_revision",
    );
    // An empty edit does not need a new revision.
    assert_accepted(&run(&mut engine, "empty", save(u64::MAX, Vec::new())));
}

#[test]
fn air_requirements_follow_overlays_and_program() {
    let mut engine = engine();
    // Off air: an on-air update is refused, a save is accepted.
    assert_refused(
        &run(
            &mut engine,
            "update-off",
            update(0, vec![(field(SCORE), FieldValue::Number(1))]),
        ),
        TitleRejectionKind::OffAir,
        RejectionCode::Conflict,
        "input",
    );
    assert_accepted(&run(
        &mut engine,
        "save",
        save(0, vec![(field(SCORE), FieldValue::Number(1))]),
    ));

    // On an overlay: a save is refused, an update is accepted.
    assert_accepted(&run(
        &mut engine,
        "overlay",
        EngineCommand::TakeOverlay {
            channel: channel(2),
            source: input(TITLE),
        },
    ));
    assert_refused(
        &run(
            &mut engine,
            "save-on",
            save(1, vec![(field(SCORE), FieldValue::Number(2))]),
        ),
        TitleRejectionKind::OnAir,
        RejectionCode::Conflict,
        "input",
    );
    assert_accepted(&run(
        &mut engine,
        "update-on",
        update(1, vec![(field(SCORE), FieldValue::Number(2))]),
    ));
    assert_accepted(&run(
        &mut engine,
        "overlay-off",
        EngineCommand::OverlayOff {
            channel: channel(2),
        },
    ));

    // Preview is not on air.
    assert_accepted(&run(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(TITLE)),
    ));
    assert_accepted(&run(&mut engine, "save-preview", save(2, Vec::new())));

    // Program is on air.
    assert_accepted(&run(&mut engine, "cut", EngineCommand::Cut));
    assert_eq!(engine.show().desired_switcher().program(), input(TITLE));
    assert_refused(
        &run(&mut engine, "save-program", save(2, Vec::new())),
        TitleRejectionKind::OnAir,
        RejectionCode::Conflict,
        "input",
    );
    assert_accepted(&run(
        &mut engine,
        "update-program",
        update(2, vec![(field(NAME), text("Live"))]),
    ));
    assert_eq!(revision(&engine), 3);
}

#[test]
fn a_save_is_refused_while_the_title_fades_out() {
    let mut engine = engine();
    fade_overlay(&mut engine, "fade", 1, 3);
    assert_accepted(&execute(&mut engine, "take", take(1, 0, Vec::new())));
    ticks(&mut engine, 4);
    assert!(realized_shows(&engine, 1));

    assert_accepted(&execute(&mut engine, "off", off_air(TITLE)));
    // Desired is off at once; the realized switcher has not even started.
    assert!(!desired_shows(&engine, 1));
    assert!(realized_shows(&engine, 1));
    assert_refused(
        &execute(&mut engine, "save-pending", save(0, Vec::new())),
        TitleRejectionKind::OnAir,
        RejectionCode::Conflict,
        "input",
    );
    engine.tick().unwrap();
    assert!(
        engine
            .realized_switcher()
            .overlay(channel(1))
            .is_transitioning()
    );
    assert_refused(
        &execute(&mut engine, "save-fading", save(0, Vec::new())),
        TitleRejectionKind::OnAir,
        RejectionCode::Conflict,
        "input",
    );
    // While fading out, the title is still on air for live updates.
    assert_accepted(&execute(
        &mut engine,
        "update-fading",
        update(0, vec![(field(NAME), text("Bye"))]),
    ));
    ticks(&mut engine, 4);
    assert!(
        !engine
            .realized_switcher()
            .overlay(channel(1))
            .is_transitioning()
    );
    assert!(!realized_shows(&engine, 1));
    assert_accepted(&run(&mut engine, "save-after", save(1, Vec::new())));
}

#[test]
fn take_applies_fields_before_the_overlay_realizes() {
    let mut engine = engine();
    let outcome = execute(
        &mut engine,
        "take",
        take(
            2,
            0,
            vec![
                (field(NAME), text("Katherine Johnson")),
                (field(FILL), FieldValue::Color(Color::new(0, 80, 160, 255))),
            ],
        ),
    );
    assert_accepted(&outcome);
    assert_eq!(
        payloads(&outcome),
        vec![EngineEvent::DesiredSwitcherChanged(take(
            2,
            0,
            vec![
                (field(NAME), text("Katherine Johnson")),
                (field(FILL), FieldValue::Color(Color::new(0, 80, 160, 255))),
            ],
        ))]
    );
    // At acceptance the fields are current and desired shows the title, but
    // nothing has reached the realized switcher yet.
    assert_eq!(revision(&engine), 1);
    assert_eq!(
        fields(&engine, TITLE).value(field(NAME)),
        Some(&text("Katherine Johnson"))
    );
    assert!(desired_shows(&engine, 2));
    assert!(!realized_shows(&engine, 2));

    let frame = engine.tick().unwrap();
    assert!(realized_shows(&engine, 2));
    assert_eq!(
        frame.overlays[channel(2).index()].source(),
        Some(input(TITLE))
    );
    assert_eq!(revision(&engine), 1);
    assert_eq!(engine.revision().get(), engine.runtime_generation().get());
}

#[test]
fn take_refuses_an_on_air_title_and_stale_fields_atomically() {
    let mut engine = engine();
    // A stale take neither changes fields nor puts the title on air.
    let stale = run(
        &mut engine,
        "stale",
        take(1, 5, vec![(field(NAME), text("x"))]),
    );
    assert_refused(
        &stale,
        TitleRejectionKind::RevisionConflict,
        RejectionCode::Conflict,
        "base_revision",
    );
    assert!(!desired_shows(&engine, 1));
    assert!(!realized_shows(&engine, 1));
    assert_eq!(revision(&engine), 0);

    let not_a_title = run(
        &mut engine,
        "plain",
        EngineCommand::TakeTitle {
            input: input(PLAIN),
            channel: channel(1),
            base_revision: 0,
            fields: Vec::new(),
        },
    );
    assert_refused(
        &not_a_title,
        TitleRejectionKind::UnknownTitle,
        RejectionCode::NotFound,
        "input",
    );

    assert_accepted(&run(
        &mut engine,
        "take",
        take(1, 0, vec![(field(NAME), text("On"))]),
    ));
    let again = run(
        &mut engine,
        "again",
        take(3, 1, vec![(field(NAME), text("Twice"))]),
    );
    assert_refused(
        &again,
        TitleRejectionKind::OnAir,
        RejectionCode::Conflict,
        "input",
    );
    assert!(!desired_shows(&engine, 3));
    assert_eq!(revision(&engine), 1);

    // A title on Program is on air too.
    let mut program = engine_with(show());
    assert_accepted(&run(
        &mut program,
        "preview",
        EngineCommand::SelectPreview(input(TITLE)),
    ));
    assert_accepted(&run(&mut program, "cut", EngineCommand::Cut));
    assert_refused(
        &run(&mut program, "take", take(1, 0, Vec::new())),
        TitleRejectionKind::OnAir,
        RejectionCode::Conflict,
        "input",
    );
}

#[test]
fn take_is_refused_while_a_fade_out_is_still_realizing() {
    let mut engine = engine();
    fade_overlay(&mut engine, "fade", 1, 3);
    assert_accepted(&run(&mut engine, "take", take(1, 0, Vec::new())));
    ticks(&mut engine, 3);
    assert_accepted(&run(&mut engine, "off", off_air(TITLE)));
    assert!(
        engine
            .realized_switcher()
            .overlay(channel(1))
            .is_transitioning()
    );
    assert_refused(
        &execute(&mut engine, "retake", take(2, 0, Vec::new())),
        TitleRejectionKind::OnAir,
        RejectionCode::Conflict,
        "input",
    );
    ticks(&mut engine, 3);
    assert_accepted(&run(&mut engine, "retake-after", take(2, 0, Vec::new())));
    assert!(realized_shows(&engine, 2));
}

#[test]
fn off_air_clears_the_single_showing_channel() {
    let mut engine = engine();
    assert_refused(
        &run(&mut engine, "not-on", off_air(TITLE)),
        TitleRejectionKind::OffAir,
        RejectionCode::Conflict,
        "input",
    );
    assert_refused(
        &run(&mut engine, "plain", off_air(PLAIN)),
        TitleRejectionKind::UnknownTitle,
        RejectionCode::NotFound,
        "input",
    );

    assert_accepted(&run(&mut engine, "take", take(3, 0, Vec::new())));
    // Another title on another channel is unaffected.
    assert_accepted(&run(
        &mut engine,
        "other",
        EngineCommand::TakeTitle {
            input: input(OTHER_TITLE),
            channel: channel(1),
            base_revision: 0,
            fields: Vec::new(),
        },
    ));
    let outcome = execute(&mut engine, "off", off_air(TITLE));
    assert_accepted(&outcome);
    assert_eq!(
        payloads(&outcome),
        vec![EngineEvent::DesiredSwitcherChanged(off_air(TITLE))]
    );
    assert!(!desired_shows(&engine, 3));
    assert!(realized_shows(&engine, 3));
    engine.tick().unwrap();
    assert!(!realized_shows(&engine, 3));
    let other = engine.realized_switcher().overlay(channel(1));
    assert!(other.is_active());
    assert_eq!(other.source(), Some(input(OTHER_TITLE)));

    // Already off: a second off-air is refused.
    assert_refused(
        &run(&mut engine, "off-again", off_air(TITLE)),
        TitleRejectionKind::OffAir,
        RejectionCode::Conflict,
        "input",
    );
}

#[test]
fn off_air_refuses_program_and_multiple_channels() {
    let mut engine = engine();
    for (key, on) in [("one", 1), ("two", 2)] {
        assert_accepted(&run(
            &mut engine,
            key,
            EngineCommand::TakeOverlay {
                channel: channel(on),
                source: input(TITLE),
            },
        ));
    }
    assert_refused(
        &run(&mut engine, "off-multiple", off_air(TITLE)),
        TitleRejectionKind::OnMultipleOverlays,
        RejectionCode::Conflict,
        "input",
    );
    assert!(desired_shows(&engine, 1) && desired_shows(&engine, 2));

    let mut program = engine_with(show());
    assert_accepted(&run(
        &mut program,
        "preview",
        EngineCommand::SelectPreview(input(TITLE)),
    ));
    assert_accepted(&run(&mut program, "cut", EngineCommand::Cut));
    assert_refused(
        &run(&mut program, "off-program", off_air(TITLE)),
        TitleRejectionKind::OnProgram,
        RejectionCode::Conflict,
        "input",
    );
    // Program wins even when an overlay also shows the title.
    assert_accepted(&run(
        &mut program,
        "overlay",
        EngineCommand::TakeOverlay {
            channel: channel(1),
            source: input(TITLE),
        },
    ));
    assert_refused(
        &run(&mut program, "off-both", off_air(TITLE)),
        TitleRejectionKind::OnProgram,
        RejectionCode::Conflict,
        "input",
    );
}

#[test]
fn title_commands_run_during_a_program_transition() {
    let mut engine = engine();
    assert_accepted(&run(
        &mut engine,
        "fade",
        EngineCommand::Fade {
            duration_frames: 10,
        },
    ));
    // Ordinary switching is refused while the transition is in flight...
    let busy = run(
        &mut engine,
        "busy",
        EngineCommand::SelectPreview(input(PLAIN)),
    );
    assert_eq!(rejection(&busy).code, RejectionCode::Conflict);
    // ...but title content and title overlays are not.
    assert_accepted(&run(
        &mut engine,
        "save",
        save(0, vec![(field(NAME), text("Mid"))]),
    ));
    assert_accepted(&run(&mut engine, "take", take(1, 1, Vec::new())));
    assert_accepted(&run(
        &mut engine,
        "update",
        update(1, vec![(field(SCORE), FieldValue::Number(9))]),
    ));
    assert_accepted(&run(&mut engine, "off", off_air(TITLE)));
    // The transition endpoints themselves are on air: Program (1) fades to
    // Preview (2), neither of which is a title, so nothing else changed.
    ticks(&mut engine, 10);
    assert_eq!(engine.realized_switcher().program(), input(PREVIEW));
    assert_eq!(revision(&engine), 2);
}

#[test]
fn a_title_in_a_transition_is_on_air() {
    let mut engine = engine();
    assert_accepted(&run(
        &mut engine,
        "preview",
        EngineCommand::SelectPreview(input(TITLE)),
    ));
    assert_accepted(&run(
        &mut engine,
        "fade",
        EngineCommand::Fade { duration_frames: 5 },
    ));
    assert!(engine.realized_switcher().transition().is_some());
    assert_refused(
        &run(&mut engine, "save", save(0, Vec::new())),
        TitleRejectionKind::OnAir,
        RejectionCode::Conflict,
        "input",
    );
    assert_refused(
        &run(&mut engine, "off", off_air(TITLE)),
        TitleRejectionKind::OnProgram,
        RejectionCode::Conflict,
        "input",
    );
}

#[test]
fn a_title_playing_as_stinger_media_is_on_air() {
    let mut engine = engine();
    let slot = fm_switcher::StingerSlotId::new(1).unwrap();
    assert_accepted(&run(
        &mut engine,
        "stinger",
        EngineCommand::ConfigureStinger {
            slot,
            descriptor: fm_switcher::StingerDescriptor {
                media_input: input(TITLE),
                preload: true,
                cut_point_frames: 2,
                audio_policy: fm_switcher::StingerAudioPolicy::Muted,
                missing_media_fallback: fm_switcher::MissingMediaFallback::Cut,
            },
        },
    ));
    // A configured but idle Stinger does not put its media on air.
    assert_accepted(&run(
        &mut engine,
        "idle-save",
        save(0, vec![(field(NAME), text("Idle"))]),
    ));
    assert_accepted(&run(
        &mut engine,
        "play",
        EngineCommand::Stinger {
            slot,
            duration_frames: 5,
        },
    ));
    assert!(engine.realized_switcher().transition().is_some());
    assert_refused(
        &run(&mut engine, "save", save(1, Vec::new())),
        TitleRejectionKind::OnAir,
        RejectionCode::Conflict,
        "input",
    );
    assert_refused(
        &run(&mut engine, "take", take(1, 1, Vec::new())),
        TitleRejectionKind::OnAir,
        RejectionCode::Conflict,
        "input",
    );
    assert_accepted(&run(
        &mut engine,
        "live",
        update(1, vec![(field(NAME), text("Playing"))]),
    ));
    ticks(&mut engine, 5);
    assert!(engine.realized_switcher().transition().is_none());
    assert_accepted(&run(&mut engine, "after", save(2, Vec::new())));
}

#[test]
fn replaying_a_title_command_returns_the_same_receipt() {
    let mut engine = engine();
    let command = save(0, vec![(field(NAME), text("Once"))]);
    let first = run(&mut engine, "save", command.clone());
    assert_accepted(&first);
    let replay = run(&mut engine, "save", command);
    assert!(replay.replayed);
    assert_eq!(replay.receipt, first.receipt);
    assert_eq!(replay.events.len(), 0);
    assert_eq!(revision(&engine), 1);

    let refused = run(&mut engine, "stale", save(0, Vec::new()));
    let refused_replay = run(&mut engine, "stale", save(0, Vec::new()));
    assert!(refused_replay.replayed);
    assert_eq!(refused_replay.receipt, refused.receipt);

    let take_first = run(&mut engine, "take", take(1, 1, Vec::new()));
    let take_replay = run(&mut engine, "take", take(1, 1, Vec::new()));
    assert!(take_replay.replayed);
    assert_eq!(take_replay.receipt, take_first.receipt);
    assert!(realized_shows(&engine, 1));
}

#[test]
fn snapshots_round_trip_title_revisions_and_values() {
    let mut engine = engine();
    assert_accepted(&run(
        &mut engine,
        "save",
        save(
            0,
            vec![
                (field(NAME), text("Snapshot")),
                (
                    field(LOGO),
                    FieldValue::Image(ImageSource::new("asset://other.png")),
                ),
            ],
        ),
    ));
    assert_accepted(&run(
        &mut engine,
        "take",
        take(2, 1, vec![(field(SCORE), FieldValue::Number(42))]),
    ));
    let snapshot = engine.snapshot().unwrap();
    assert_eq!(
        snapshot
            .show()
            .title(input(TITLE))
            .unwrap()
            .fields
            .revision(),
        2
    );

    let mut restored = Engine::restore(snapshot.clone()).unwrap();
    assert_eq!(restored.show(), engine.show());
    assert_eq!(revision(&restored), 2);
    assert_eq!(
        fields(&restored, TITLE).value(field(SCORE)),
        Some(&FieldValue::Number(42))
    );
    assert_eq!(fields(&restored, OTHER_TITLE), fields(&engine, OTHER_TITLE));
    assert_eq!(restored.snapshot().unwrap(), snapshot);

    // The restored title keeps its revision for compare-and-set.
    assert_refused(
        &run(&mut restored, "stale", update(1, Vec::new())),
        TitleRejectionKind::RevisionConflict,
        RejectionCode::Conflict,
        "base_revision",
    );
    assert_accepted(&run(
        &mut restored,
        "live",
        update(2, vec![(field(NAME), text("Restored"))]),
    ));
    assert_eq!(revision(&restored), 3);
    // Replays of keys retained across the snapshot keep their receipts.
    let replay = run(&mut restored, "save", save(0, Vec::new()));
    assert!(replay.replayed);
    assert!(replay.receipt.accepted().is_some());
}
