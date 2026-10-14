use fm_automation::{
    Chord, CommandIntent, InputSelector, KeyStroke, MAX_PROFILE_LINE_BYTES,
    MAX_TEMPLATE_NAME_BYTES, MAX_TEMPLATES, Modifiers, Shortcut, ShortcutConflict, ShortcutError,
    ShortcutProfileCodec, ShortcutProfileError, ShortcutRegistry, ShortcutScope, ShortcutTemplate,
    ShortcutTemplateRegistry, TemplateError, TemplateRegistryError, TemplateTarget, ValueRange,
    parse_shortcut_profile, render_shortcut_profile,
};
use fm_types::InputId;
use std::fmt::Write as _;
use std::num::NonZeroU128;

#[derive(Clone, Debug, Eq, PartialEq)]
enum TestCommand {
    Named(String),
    Value(i32),
}

struct TestCodec;

impl ShortcutProfileCodec<TestCommand> for TestCodec {
    fn encode_command(&self, command: &TestCommand) -> String {
        match command {
            TestCommand::Named(name) => format!("name:{name}"),
            TestCommand::Value(value) => format!("value:{value}"),
        }
    }

    fn decode_command(&self, text: &str) -> Result<TestCommand, String> {
        if let Some(name) = text.strip_prefix("name:") {
            return Ok(TestCommand::Named(name.to_owned()));
        }
        if let Some(value) = text.strip_prefix("value:") {
            return value
                .parse()
                .map(TestCommand::Value)
                .map_err(|error: std::num::ParseIntError| error.to_string());
        }
        Err(format!("unknown command {text}"))
    }
}

struct FailingCodec;

impl ShortcutProfileCodec<TestCommand> for FailingCodec {
    fn encode_command(&self, _command: &TestCommand) -> String {
        "unavailable".to_owned()
    }

    fn decode_command(&self, _text: &str) -> Result<TestCommand, String> {
        Err("commands are unavailable".to_owned())
    }
}

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn chord(strokes: &[(&str, Modifiers)]) -> Chord {
    Chord::new(
        strokes
            .iter()
            .map(|(key, modifiers)| KeyStroke::new(*key, *modifiers)),
    )
    .unwrap()
}

fn simple_chord(key: &str) -> Chord {
    chord(&[(key, Modifiers::default())])
}

fn static_template(id: &str, scope: ShortcutScope, key: &str) -> ShortcutTemplate<TestCommand> {
    static_template_with_chord(id, scope, simple_chord(key))
}

fn static_template_with_chord(
    id: &str,
    scope: ShortcutScope,
    chord: Chord,
) -> ShortcutTemplate<TestCommand> {
    ShortcutTemplate::new(
        id.to_owned(),
        format!("{id} name"),
        scope,
        chord,
        CommandIntent::discrete(TestCommand::Named(id.to_owned())),
        TemplateTarget::Static,
    )
    .unwrap()
}

fn shortcut(id: &str, scope: ShortcutScope, chord: Chord) -> Shortcut<TestCommand> {
    Shortcut {
        id: id.to_owned(),
        scope,
        chord,
        intent: CommandIntent::discrete(TestCommand::Named(id.to_owned())),
    }
}

fn shortcut_conflicts(error: ShortcutError) -> Vec<ShortcutConflict> {
    match error {
        ShortcutError::Conflict(conflicts) => conflicts,
        other @ ShortcutError::DuplicateId(_) => {
            panic!("expected a shortcut conflict, got {other}")
        }
    }
}

fn template_conflicts(error: TemplateRegistryError) -> Vec<ShortcutConflict> {
    match error {
        TemplateRegistryError::Conflict(conflicts) => conflicts,
        other => panic!("expected a template conflict, got {other}"),
    }
}

fn sample_templates() -> Vec<ShortcutTemplate<TestCommand>> {
    vec![
        ShortcutTemplate::new(
            "global-go".to_owned(),
            "Global GO".to_owned(),
            ShortcutScope::Global,
            chord(&[("G", Modifiers::default())]),
            CommandIntent::discrete(TestCommand::Named("go".to_owned())),
            TemplateTarget::Static,
        )
        .unwrap(),
        ShortcutTemplate::new(
            "audio-fader".to_owned(),
            "Audio fader".to_owned(),
            ShortcutScope::Local("audio".to_owned()),
            chord(&[("F", Modifiers::CONTROL | Modifiers::SHIFT)]),
            CommandIntent::continuous("fader-1".to_owned(), TestCommand::Named("fade".to_owned())),
            TemplateTarget::Value {
                stream: "fader-1".to_owned(),
                range: ValueRange::new(-60.0, 12.0).unwrap(),
            },
        )
        .unwrap(),
        ShortcutTemplate::new(
            "preview-3".to_owned(),
            "Preview third input".to_owned(),
            ShortcutScope::Local("switcher".to_owned()),
            chord(&[("3", Modifiers::ALT), ("P", Modifiers::META)]),
            CommandIntent::commit_continuous("preview".to_owned(), TestCommand::Value(3)),
            TemplateTarget::Input(InputSelector::BySlot { slot: 3 }),
        )
        .unwrap(),
        ShortcutTemplate::new(
            "fixed-9".to_owned(),
            "Fixed ninth input".to_owned(),
            ShortcutScope::Global,
            chord(&[("Esc", Modifiers::default()), ("9", Modifiers::default())]),
            CommandIntent::discrete(TestCommand::Named("cancel".to_owned())),
            TemplateTarget::Input(InputSelector::ByInput(input(9))),
        )
        .unwrap(),
        ShortcutTemplate::new(
            "all-mods".to_owned(),
            "All modifiers".to_owned(),
            ShortcutScope::Global,
            chord(&[(
                "Key",
                Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT | Modifiers::META,
            )]),
            CommandIntent::discrete(TestCommand::Named("everything".to_owned())),
            TemplateTarget::Static,
        )
        .unwrap(),
        ShortcutTemplate::new(
            "escape%:id".to_owned(),
            "50% fade: in".to_owned(),
            ShortcutScope::Local("scope%:,/1".to_owned()),
            chord(&[
                ("a+b", Modifiers::default()),
                ("c:d,e%f", Modifiers::default()),
            ]),
            CommandIntent::discrete(TestCommand::Named("escape".to_owned())),
            TemplateTarget::Static,
        )
        .unwrap(),
    ]
}

fn many_templates(count: usize) -> Vec<ShortcutTemplate<TestCommand>> {
    (0..count)
        .map(|index| {
            let number = u32::try_from(index).unwrap();
            let scope = if index % 2 == 0 {
                ShortcutScope::Global
            } else {
                ShortcutScope::Local(format!("scope-{index}"))
            };
            let command = TestCommand::Value(i32::try_from(index).unwrap());
            let intent = match index % 3 {
                0 => CommandIntent::discrete(command),
                1 => CommandIntent::continuous(format!("stream-{index}"), command),
                _ => CommandIntent::commit_continuous(format!("stream-{index}"), command),
            };
            let target = match index % 3 {
                0 => TemplateTarget::Static,
                1 => TemplateTarget::Input(InputSelector::BySlot {
                    slot: u16::try_from(index + 1).unwrap(),
                }),
                _ => TemplateTarget::Value {
                    stream: format!("value-{index}"),
                    range: ValueRange::new(f64::from(number), f64::from(number) + 1.0).unwrap(),
                },
            };
            ShortcutTemplate::new(
                format!("many-{index}"),
                format!("Many {index}"),
                scope,
                simple_chord("K"),
                intent,
                target,
            )
            .unwrap()
        })
        .collect()
}

#[test]
fn slot_targets_resolve_against_the_current_roster() {
    let mut registry = ShortcutTemplateRegistry::default();
    registry
        .insert(
            ShortcutTemplate::new(
                "preview-2".to_owned(),
                "Preview second input".to_owned(),
                ShortcutScope::Local("switcher".to_owned()),
                simple_chord("2"),
                CommandIntent::continuous(
                    "preview".to_owned(),
                    TestCommand::Named("preview".to_owned()),
                ),
                TemplateTarget::Input(InputSelector::BySlot { slot: 2 }),
            )
            .unwrap(),
        )
        .unwrap();

    let first = [input(11), input(22), input(33)];
    let press = registry
        .press("preview-2", Some("switcher"), &first, None)
        .unwrap();
    assert_eq!(press.resolved_input, Some(input(22)));
    assert_eq!(press.resolved_value, None);
    assert_eq!(press.template_id, "preview-2");

    // The roster changes; the same press resolves the new identity in that slot.
    let second = [input(44), input(55)];
    let press = registry
        .press("preview-2", Some("switcher"), &second, None)
        .unwrap();
    assert_eq!(press.resolved_input, Some(input(55)));

    assert_eq!(
        registry.press("preview-2", Some("switcher"), &[input(1)], None),
        Err(TemplateError::SlotOutOfRange {
            slot: 2,
            roster_len: 1,
        })
    );
    assert_eq!(
        registry.press("preview-2", Some("switcher"), &[], None),
        Err(TemplateError::EmptyInputRoster)
    );
    assert_eq!(
        registry.press("preview-2", Some("audio"), &second, None),
        Err(TemplateError::InactiveTemplate("preview-2".to_owned()))
    );
    // An explicit identifier may bypass the scope assertion when the caller
    // knows no local scope is active.
    assert!(registry.press("preview-2", None, &second, None).is_ok());
    assert_eq!(
        registry.press("missing", None, &[], None),
        Err(TemplateError::UnknownTemplate("missing".to_owned()))
    );
}

#[test]
fn by_input_targets_never_consult_the_roster() {
    let identity = input(7);
    let mut registry = ShortcutTemplateRegistry::default();
    registry
        .insert(
            ShortcutTemplate::new(
                "fixed".to_owned(),
                "Fixed input".to_owned(),
                ShortcutScope::Global,
                simple_chord("7"),
                CommandIntent::discrete(TestCommand::Named("select".to_owned())),
                TemplateTarget::Input(InputSelector::ByInput(identity)),
            )
            .unwrap(),
        )
        .unwrap();

    assert_eq!(
        registry
            .press("fixed", None, &[], None)
            .unwrap()
            .resolved_input,
        Some(identity)
    );
    // The identity need not appear in the roster, and its order is irrelevant.
    assert_eq!(
        registry
            .press("fixed", None, &[input(1), input(2)], None)
            .unwrap()
            .resolved_input,
        Some(identity)
    );
    // An input target still refuses a press-time value.
    assert_eq!(
        registry.press("fixed", None, &[], Some(0.5)),
        Err(TemplateError::UnexpectedValue)
    );
}

#[test]
fn value_targets_validate_press_time_values() {
    let mut registry = ShortcutTemplateRegistry::default();
    registry
        .insert(
            ShortcutTemplate::new(
                "lumakey".to_owned(),
                "Lumakey".to_owned(),
                ShortcutScope::Local("audio".to_owned()),
                simple_chord("L"),
                CommandIntent::continuous(
                    "lumakey".to_owned(),
                    TestCommand::Named("set".to_owned()),
                ),
                TemplateTarget::Value {
                    stream: "lumakey".to_owned(),
                    range: ValueRange::new(0.0, 1.0).unwrap(),
                },
            )
            .unwrap(),
        )
        .unwrap();

    let press = registry
        .press("lumakey", Some("audio"), &[], Some(0.5))
        .unwrap();
    assert_eq!(press.resolved_value, Some(0.5));
    assert_eq!(press.resolved_input, None);
    assert!(
        registry
            .press("lumakey", Some("audio"), &[], Some(0.0))
            .is_ok()
    );
    assert!(
        registry
            .press("lumakey", Some("audio"), &[], Some(1.0))
            .is_ok()
    );

    assert_eq!(
        registry.press("lumakey", Some("audio"), &[], Some(f64::NAN)),
        Err(TemplateError::NonFiniteValue)
    );
    assert_eq!(
        registry.press("lumakey", Some("audio"), &[], Some(f64::INFINITY)),
        Err(TemplateError::NonFiniteValue)
    );
    assert_eq!(
        registry.press("lumakey", Some("audio"), &[], Some(1.5)),
        Err(TemplateError::ValueOutOfRange {
            value: 1.5,
            minimum: 0.0,
            maximum: 1.0,
        })
    );
    assert_eq!(
        registry.press("lumakey", Some("audio"), &[], None),
        Err(TemplateError::MissingValue)
    );
    // The ambient roster only feeds slot selectors; a value target ignores it.
    assert!(
        registry
            .press("lumakey", Some("audio"), &[input(1)], Some(0.5))
            .is_ok()
    );
}

#[test]
fn static_targets_reject_supplied_payloads() {
    let mut registry = ShortcutTemplateRegistry::default();
    registry
        .insert(static_template("go", ShortcutScope::Global, "Enter"))
        .unwrap();

    let press = registry.press("go", None, &[], None).unwrap();
    assert_eq!(press.resolved_input, None);
    assert_eq!(press.resolved_value, None);
    assert_eq!(
        registry.press("go", None, &[input(1)], None),
        Err(TemplateError::UnexpectedInput)
    );
    assert_eq!(
        registry.press("go", None, &[], Some(0.5)),
        Err(TemplateError::UnexpectedValue)
    );
}

#[test]
fn registry_conflicts_and_resolution_match_shortcut_registry() {
    let mut shortcuts = ShortcutRegistry::default();
    let mut templates = ShortcutTemplateRegistry::default();
    let audio = ShortcutScope::Local("audio".to_owned());
    let switcher = ShortcutScope::Local("switcher".to_owned());

    shortcuts
        .insert(shortcut("audio-g", audio.clone(), simple_chord("G")))
        .unwrap();
    templates
        .insert(static_template("audio-g", audio.clone(), "G"))
        .unwrap();
    shortcuts
        .insert(shortcut("switcher-g", switcher.clone(), simple_chord("G")))
        .unwrap();
    templates
        .insert(static_template("switcher-g", switcher.clone(), "G"))
        .unwrap();
    shortcuts
        .insert(shortcut(
            "global-o",
            ShortcutScope::Global,
            simple_chord("O"),
        ))
        .unwrap();
    templates
        .insert(static_template("global-o", ShortcutScope::Global, "O"))
        .unwrap();

    // Exact conflict inside one scope.
    let exact_shortcut = shortcuts.insert(shortcut("audio-g-2", audio.clone(), simple_chord("G")));
    let exact_template = templates.insert(static_template("audio-g-2", audio.clone(), "G"));
    assert_eq!(
        shortcut_conflicts(exact_shortcut.unwrap_err()),
        template_conflicts(exact_template.unwrap_err())
    );

    // Prefix conflict inside one scope.
    let prefix_chord = chord(&[("G", Modifiers::default()), ("O", Modifiers::default())]);
    let prefix_shortcut =
        shortcuts.insert(shortcut("audio-go", audio.clone(), prefix_chord.clone()));
    let prefix_template = templates.insert(static_template_with_chord(
        "audio-go",
        audio.clone(),
        prefix_chord,
    ));
    assert_eq!(
        shortcut_conflicts(prefix_shortcut.unwrap_err()),
        template_conflicts(prefix_template.unwrap_err())
    );

    // A global binding overlapping a local one conflicts exactly.
    let global_shortcut = shortcuts.insert(shortcut(
        "global-g",
        ShortcutScope::Global,
        simple_chord("G"),
    ));
    let global_template = templates.insert(static_template("global-g", ShortcutScope::Global, "G"));
    assert_eq!(
        shortcut_conflicts(global_shortcut.unwrap_err()),
        template_conflicts(global_template.unwrap_err())
    );

    // Resolution agrees on every probe, including the None-scope fallback.
    let probes = [
        (Some("audio"), simple_chord("G")),
        (Some("switcher"), simple_chord("G")),
        (None, simple_chord("G")),
        (Some("other"), simple_chord("G")),
        (Some("audio"), simple_chord("O")),
        (None, simple_chord("O")),
        (Some("other"), simple_chord("O")),
    ];
    for (scope, probe) in probes {
        assert_eq!(
            shortcuts
                .resolve(scope, &probe)
                .map(|entry| entry.id.as_str()),
            templates
                .resolve(scope, &probe)
                .map(|entry| entry.id.as_str()),
            "resolution diverged for scope {scope:?} and chord {probe:?}"
        );
    }
}

#[test]
fn duplicate_over_capacity_and_invalid_templates_are_rejected() {
    let mut registry = ShortcutTemplateRegistry::default();
    registry
        .insert(static_template("go", ShortcutScope::Global, "G"))
        .unwrap();
    assert!(matches!(
        registry.insert(static_template("go", ShortcutScope::Global, "H")),
        Err(TemplateRegistryError::DuplicateId(id)) if id == "go"
    ));

    // A template that never passed its constructor validation is rejected too.
    let mut invalid = static_template("bad", ShortcutScope::Global, "B");
    invalid.name.clear();
    assert!(matches!(
        registry.insert(invalid),
        Err(TemplateRegistryError::InvalidTemplate(
            TemplateError::EmptyName
        ))
    ));
    assert!(registry.get("bad").is_none());

    let mut full = ShortcutTemplateRegistry::default();
    for index in 0..MAX_TEMPLATES {
        full.insert(
            ShortcutTemplate::new(
                format!("id-{index}"),
                format!("Template {index}"),
                ShortcutScope::Local(format!("scope-{index}")),
                simple_chord("G"),
                CommandIntent::discrete(TestCommand::Value(i32::try_from(index).unwrap())),
                TemplateTarget::Static,
            )
            .unwrap(),
        )
        .unwrap();
    }
    assert_eq!(full.len(), MAX_TEMPLATES);
    let overflow = full.insert(static_template(
        "overflow",
        ShortcutScope::Local("extra".to_owned()),
        "H",
    ));
    assert!(matches!(
        overflow,
        Err(TemplateRegistryError::TooManyTemplates { maximum }) if maximum == MAX_TEMPLATES
    ));
}

#[test]
fn templates_are_removed_by_identifier() {
    let mut registry = ShortcutTemplateRegistry::default();
    registry
        .insert(static_template("a", ShortcutScope::Global, "A"))
        .unwrap();
    registry
        .insert(static_template("b", ShortcutScope::Global, "B"))
        .unwrap();

    let removed = registry.remove("a").unwrap();
    assert_eq!(removed.id, "a");
    assert!(registry.get("a").is_none());
    assert!(registry.remove("a").is_none());
    assert_eq!(registry.len(), 1);
    assert!(!registry.is_empty());
    registry.remove("b");
    assert!(registry.is_empty());
}

#[test]
fn profile_round_trips_every_target_and_delivery_class() {
    let codec = TestCodec;
    let empty = render_shortcut_profile::<TestCommand>(&[], &codec);
    assert_eq!(empty, "freemix-shortcuts\t1\n");
    assert!(parse_shortcut_profile(&empty, &codec).unwrap().is_empty());

    let templates = sample_templates();
    let rendered = render_shortcut_profile(&templates, &codec);
    assert!(
        rendered.contains("\tcasm:Key\t"),
        "modifiers must render in canonical order"
    );
    let parsed = parse_shortcut_profile(&rendered, &codec).unwrap();
    assert_eq!(parsed, templates);
    assert_eq!(render_shortcut_profile(&parsed, &codec), rendered);
    assert_eq!(
        parse_shortcut_profile(rendered.strip_suffix('\n').unwrap(), &codec).unwrap(),
        templates
    );

    let single = &templates[..1];
    let rendered = render_shortcut_profile(single, &codec);
    assert_eq!(
        parse_shortcut_profile(&rendered, &codec)
            .unwrap()
            .as_slice(),
        single
    );

    let many = many_templates(64);
    let rendered = render_shortcut_profile(&many, &codec);
    let parsed = parse_shortcut_profile(&rendered, &codec).unwrap();
    assert_eq!(parsed, many);
    assert_eq!(render_shortcut_profile(&parsed, &codec), rendered);

    let mut registry = ShortcutTemplateRegistry::default();
    for template in templates {
        registry.insert(template).unwrap();
    }
    let rendered = render_shortcut_profile(registry.templates(), &codec);
    assert_eq!(
        parse_shortcut_profile(&rendered, &codec)
            .unwrap()
            .as_slice(),
        registry.templates()
    );
}

#[test]
fn press_is_deterministic_and_does_not_mutate_the_registry() {
    let mut registry = ShortcutTemplateRegistry::default();
    registry
        .insert(
            ShortcutTemplate::new(
                "preview-2".to_owned(),
                "Preview second".to_owned(),
                ShortcutScope::Local("switcher".to_owned()),
                simple_chord("2"),
                CommandIntent::discrete(TestCommand::Named("preview".to_owned())),
                TemplateTarget::Input(InputSelector::BySlot { slot: 2 }),
            )
            .unwrap(),
        )
        .unwrap();
    registry
        .insert(static_template("go", ShortcutScope::Global, "Enter"))
        .unwrap();

    let before = registry.templates().to_vec();
    let roster = [input(5), input(6)];
    let first = registry
        .press("preview-2", Some("switcher"), &roster, None)
        .unwrap();
    let second = registry
        .press("preview-2", Some("switcher"), &roster, None)
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(first.resolved_input, Some(input(6)));
    assert_eq!(first.name, "Preview second");
    assert_eq!(first.chord, simple_chord("2"));
    assert_eq!(
        first.intent,
        CommandIntent::discrete(TestCommand::Named("preview".to_owned()))
    );
    assert_eq!(registry.templates(), before.as_slice());
    assert_eq!(registry.len(), 2);

    let by_chord = registry
        .resolve_chord(Some("switcher"), &simple_chord("2"), &roster, None)
        .unwrap();
    assert_eq!(by_chord, first);
    assert_eq!(
        registry
            .resolve_chord(None, &simple_chord("Enter"), &[], None)
            .unwrap()
            .template_id,
        "go"
    );
    assert_eq!(
        registry.resolve_chord(None, &simple_chord("Z"), &[], None),
        Err(TemplateError::UnknownChord)
    );
    assert_eq!(registry.templates(), before.as_slice());
}

const HEADER: &str = "freemix-shortcuts\t1";
const STATIC_LINE: &str = "go\tGo\tglobal\t-:G\tdiscrete\t\tstatic\t\tname:go";
const VALUE_LINE: &str =
    "fader\tFader\taudio\t-:F\tcontinuous\tfader-1\tvalue\tlumakey:0:1\tname:fade";
const SLOT_LINE: &str = "pad\tPad\tswitcher\t-:4\tdiscrete\t\tinput\tslot:2\tname:pad";

fn document(line: &str) -> String {
    format!("{HEADER}\n{line}\n")
}

#[track_caller]
fn assert_rejected(text: &str) -> ShortcutProfileError {
    parse_shortcut_profile(text, &TestCodec)
        .expect_err("document must be rejected wholesale, never partially parsed")
}

#[test]
fn profile_headers_are_validated() {
    assert!(matches!(
        parse_shortcut_profile("", &TestCodec),
        Err(ShortcutProfileError::MissingHeader)
    ));
    assert!(matches!(
        parse_shortcut_profile(&format!("other\t1\n{STATIC_LINE}\n"), &TestCodec),
        Err(ShortcutProfileError::UnknownHeader(_))
    ));
    let unsupported = parse_shortcut_profile(
        &format!("freemix-shortcuts\t2\n{STATIC_LINE}\n"),
        &TestCodec,
    )
    .unwrap_err();
    assert!(matches!(
        unsupported,
        ShortcutProfileError::UnsupportedVersion(version) if version == "2"
    ));
    assert!(
        parse_shortcut_profile(&format!("{HEADER}\n"), &TestCodec)
            .unwrap()
            .is_empty()
    );
    let long_header = "x".repeat(MAX_PROFILE_LINE_BYTES + 1);
    assert!(matches!(
        parse_shortcut_profile(&format!("{long_header}\n"), &TestCodec),
        Err(ShortcutProfileError::LineTooLong { line: 1, .. })
    ));
}

#[test]
fn profile_fields_and_chords_are_validated() {
    // Unknown scope, target, and delivery class.
    assert_rejected(&document(&STATIC_LINE.replace("global", "galactic")));
    assert_rejected(&document(&STATIC_LINE.replace("\tstatic\t", "\tswirl\t")));
    assert_rejected(&document(&STATIC_LINE.replace("\tdiscrete\t", "\tfancy\t")));
    assert_rejected(&document(
        &STATIC_LINE.replace("\tdiscrete\t\t", "\tdiscrete\tx\t"),
    ));

    // Malformed chords and modifiers.
    assert_rejected(&document(&STATIC_LINE.replace("-:G", "x:G")));
    assert_rejected(&document(&STATIC_LINE.replace("-:G", "sc:G")));
    assert_rejected(&document(&STATIC_LINE.replace("-:G", "cc:G")));
    assert_rejected(&document(&STATIC_LINE.replace("-:G", "G")));
    assert_rejected(&document("-:A,-:B,-:C,-:D,-:E"));

    // Field count, blank lines, oversized lines, and raw carriage returns.
    assert_rejected(&format!("{HEADER}\n{STATIC_LINE}\textra\n"));
    assert_rejected(&format!(
        "{HEADER}\n{}\n",
        STATIC_LINE.rsplit_once('\t').unwrap().0
    ));
    assert_rejected(&format!("{HEADER}\n\n"));
    assert_rejected(&format!(
        "{HEADER}\n{}\n",
        "x".repeat(MAX_PROFILE_LINE_BYTES + 1)
    ));
    assert_rejected(&format!("{HEADER}\n{STATIC_LINE}\r\n"));

    // Empty, oversized, and non-canonically escaped identifiers and names.
    assert_rejected(&document(&STATIC_LINE.replacen("go", "", 1)));
    assert_rejected(&document(&STATIC_LINE.replace("go\tGo", "%\tGo")));
    assert_rejected(&document(&STATIC_LINE.replacen("Go", "", 1)));
    assert_rejected(&format!(
        "{HEADER}\n{}\tGo\tglobal\t-:G\tdiscrete\t\tstatic\t\tname:go\n",
        "x".repeat(MAX_TEMPLATE_NAME_BYTES + 1)
    ));
    assert_rejected(&format!(
        "{HEADER}\ngo\t{}\tglobal\t-:G\tdiscrete\t\tstatic\t\tname:go\n",
        "x".repeat(MAX_TEMPLATE_NAME_BYTES + 1)
    ));

    // A local scope name is required.
    assert_rejected(&document(&STATIC_LINE.replace("global", "local:")));
}

#[test]
fn profile_targets_and_ranges_are_validated() {
    // Value payloads must be finite and increasing.
    assert_rejected(&document(
        &VALUE_LINE.replace("lumakey:0:1", "lumakey:inf:1"),
    ));
    assert_rejected(&document(
        &VALUE_LINE.replace("lumakey:0:1", "lumakey:NaN:1"),
    ));
    assert_rejected(&document(&VALUE_LINE.replace("lumakey:0:1", "lumakey:1:0")));
    assert_rejected(&document(&VALUE_LINE.replace("lumakey:0:1", "lumakey:1:1")));
    assert_rejected(&document(
        &VALUE_LINE.replace("lumakey:0:1", "lumakey:0:one"),
    ));
    assert_rejected(&document(&VALUE_LINE.replace("lumakey:0:1", "lumakey:0")));
    assert_rejected(&document(
        &VALUE_LINE.replace("lumakey:0:1", "lumakey:1.0:2"),
    ));
    assert_rejected(&document(
        &VALUE_LINE.replace("lumakey:0:1", "lumakey::0:1"),
    ));

    // A static target accepts no payload at all.
    assert_rejected(&document(&VALUE_LINE.replace("\tvalue\t", "\tstatic\t")));

    // Input selectors are 1-based, canonical, and bounded.
    assert_rejected(&document(&SLOT_LINE.replace("slot:2", "slot:0")));
    assert_rejected(&document(&SLOT_LINE.replace("slot:2", "slot:02")));
    assert_rejected(&document(&SLOT_LINE.replace("slot:2", "slot:70000")));
    assert_rejected(&document(&SLOT_LINE.replace("slot:2", "input:0")));
    assert_rejected(&document(&SLOT_LINE.replace("slot:2", "input:07")));
    assert_rejected(&document(&SLOT_LINE.replace("slot:2", "")));
    assert_rejected(&document(&SLOT_LINE.replace("slot:2", "wat:2")));
}

#[test]
fn profile_documents_reject_duplicates_capacity_and_codec_failures() {
    // Duplicate ids are rejected even when the chords differ.
    let duplicate = parse_shortcut_profile(
        &format!(
            "{HEADER}\n{STATIC_LINE}\n{}\n",
            STATIC_LINE.replace("-:G", "-:H")
        ),
        &TestCodec,
    );
    assert!(matches!(
        duplicate,
        Err(ShortcutProfileError::DuplicateId { line: 3, id }) if id == "go"
    ));

    // A codec failure rejects the document.
    let rejected = assert_rejected(&document(&STATIC_LINE.replace("name:go", "bogus")));
    assert!(matches!(
        rejected,
        ShortcutProfileError::InvalidCommand { line: 2, .. }
    ));

    // A raw tab or newline inside the encoded command breaks the field or line
    // structure and is rejected.
    assert_rejected(&document(&STATIC_LINE.replace("name:go", "name:a\tb")));
    assert_rejected(&document(&STATIC_LINE.replace("name:go", "name:a\nb")));

    // Wholesale rejection: a valid first line never yields a partial result.
    assert_rejected(&format!(
        "{HEADER}\n{STATIC_LINE}\n{}\n",
        STATIC_LINE.replace("discrete", "fancy")
    ));

    // The codec decides whether an opaque command field is usable.
    assert!(parse_shortcut_profile(&document(STATIC_LINE), &FailingCodec).is_err());

    // The line count is bounded at MAX_TEMPLATES.
    let mut at_capacity = String::from(HEADER);
    at_capacity.push('\n');
    for index in 0..MAX_TEMPLATES {
        writeln!(
            at_capacity,
            "id-{index}\tName {index}\tglobal\t-:G\tdiscrete\t\tstatic\t\tname:{index}"
        )
        .unwrap();
    }
    assert_eq!(
        parse_shortcut_profile(&at_capacity, &TestCodec)
            .unwrap()
            .len(),
        MAX_TEMPLATES
    );
    at_capacity.push_str("extra\tExtra\tglobal\t-:H\tdiscrete\t\tstatic\t\tname:extra\n");
    assert!(matches!(
        parse_shortcut_profile(&at_capacity, &TestCodec),
        Err(ShortcutProfileError::TooManyTemplates { maximum }) if maximum == MAX_TEMPLATES
    ));
}
