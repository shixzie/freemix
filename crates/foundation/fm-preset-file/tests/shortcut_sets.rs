//! The strict `.fmshortcuts` file that shares a shortcut set between
//! projects and consoles (RC-004): the exact document, refusals, and the
//! plan that merges a set into a project's shortcuts.

use core::num::NonZeroU128;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_preset_file::shortcuts::{
    ShortcutImportError, ShortcutSetFile, decode_shortcut_set_file, encode_shortcut_set_file,
    plan_shortcut_import, read_shortcut_set_file, write_shortcut_set_file,
};
use fm_preset_file::{PresetFileError, SCHEMA_VERSION};
use fm_protocol::{
    MAX_SHORTCUTS, ShortcutSpec, ShortcutStatus, TransitionFavoriteEffect, WireCueAction,
    WireInputId, WireOverlayChannelId, WireShortcutAction, WireShortcutCueOperation,
    WireShortcutKey,
};

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-shortcut-set-{}-{sequence}-{name}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn key(name: &str, shift: bool) -> WireShortcutKey {
    WireShortcutKey {
        key: name.to_owned(),
        command: false,
        shift,
        alt: false,
        then: None,
        scope: None,
    }
}

fn input(value: u128) -> WireInputId {
    WireInputId::new(NonZeroU128::new(value).unwrap())
}

fn spec(name: &str, key: Option<WireShortcutKey>, osc: Option<&str>) -> ShortcutSpec {
    ShortcutSpec {
        name: name.to_owned(),
        enabled: true,
        key,
        osc: osc.map(str::to_owned),
        action: WireShortcutAction::Cue {
            list: 1,
            operation: WireShortcutCueOperation::Go { cue: None },
        },
    }
}

fn status(id: u64, spec: ShortcutSpec) -> ShortcutStatus {
    ShortcutStatus { id, spec }
}

fn sample() -> ShortcutSetFile {
    ShortcutSetFile {
        name: "Studio \"A\"".to_owned(),
        shortcuts: vec![
            ShortcutSpec {
                name: "Intro".to_owned(),
                enabled: true,
                key: Some(key("F9", true)),
                osc: Some("intro".to_owned()),
                action: WireShortcutAction::Actions(vec![
                    WireCueAction::OverlayOn {
                        channel: WireOverlayChannelId::new(1).unwrap(),
                        input: fm_types::InputTarget::Fixed(input(4)),
                    },
                    WireCueAction::Take {
                        input: Some(fm_types::InputTarget::Fixed(input(3))),
                        effect: TransitionFavoriteEffect::Cut,
                        duration_frames: 0,
                    },
                ]),
            },
            ShortcutSpec {
                name: "Open".to_owned(),
                enabled: false,
                key: None,
                osc: None,
                action: WireShortcutAction::Cue {
                    list: 1,
                    operation: WireShortcutCueOperation::Go { cue: Some(2) },
                },
            },
            ShortcutSpec {
                name: "Scores".to_owned(),
                enabled: true,
                // Ctrl+K then 1, in the Cues panel.
                key: Some(WireShortcutKey {
                    then: Some(Box::new(key("1", false))),
                    scope: Some("cues".to_owned()),
                    ..WireShortcutKey::stroke("K", true, false, false)
                }),
                osc: None,
                action: WireShortcutAction::Script { script: 2 },
            },
        ],
    }
}

#[test]
fn a_shortcut_set_encodes_exactly_and_round_trips() {
    let set = sample();
    let text = encode_shortcut_set_file(&set).unwrap();
    assert_eq!(
        text,
        format!(
            "{{\"schema_version\": {SCHEMA_VERSION}, \"kind\": \"shortcut_set\", \
             \"name\": \"Studio \\\"A\\\"\", \"shortcuts\": [\n  \
             {{\"name\": \"Intro\", \"enabled\": true, \"key\": {{\"key\": \"F9\", \
             \"command\": false, \"shift\": true, \"alt\": false, \"then\": null, \
             \"scope\": null}}, \"osc\": \"intro\", \
             \"action\": \"actions:overlay_on.1.4/take.3.cut.0\"}},\n  \
             {{\"name\": \"Open\", \"enabled\": false, \"key\": null, \"osc\": null, \
             \"action\": \"cue:1:go:2\"}},\n  \
             {{\"name\": \"Scores\", \"enabled\": true, \"key\": {{\"key\": \"K\", \
             \"command\": true, \"shift\": false, \"alt\": false, \"then\": {{\"key\": \"1\", \
             \"command\": false, \"shift\": false, \"alt\": false}}, \"scope\": \"cues\"}}, \
             \"osc\": null, \
             \"action\": \"script:2\"}}\n]}}\n"
        )
    );
    assert_eq!(decode_shortcut_set_file(&text).unwrap(), set);

    let directory = TestDirectory::new("round-trip");
    let path = directory.path("studio.fmshortcuts");
    write_shortcut_set_file(&path, &set).unwrap();
    assert_eq!(read_shortcut_set_file(&path).unwrap(), set);
}

#[test]
fn a_shortcut_set_is_refused_unless_strict_and_valid() {
    let text = encode_shortcut_set_file(&sample()).unwrap();
    // Another schema, another kind, an unknown key.
    let other_schema = text.replacen(
        &format!("\"schema_version\": {SCHEMA_VERSION}"),
        "\"schema_version\": 1",
        1,
    );
    assert!(matches!(
        decode_shortcut_set_file(&other_schema),
        Err(PresetFileError::UnsupportedSchema { found: 1, .. })
    ));
    let other_kind = text.replacen("\"shortcut_set\"", "\"grade\"", 1);
    assert!(matches!(
        decode_shortcut_set_file(&other_kind),
        Err(PresetFileError::WrongKind { .. })
    ));
    let extra = text.replacen("\"enabled\": false,", "\"enabled\": false, \"id\": 2,", 1);
    assert!(matches!(
        decode_shortcut_set_file(&extra),
        Err(PresetFileError::Malformed { .. })
    ));
    // An action outside the notation, or one that waits.
    for action in ["cue:0:stop", "actions:wait.100", "macro:1", "script:x"] {
        let bad = text.replacen("script:2", action, 1);
        assert!(
            matches!(
                decode_shortcut_set_file(&bad),
                Err(PresetFileError::Invalid(_))
            ),
            "{action}"
        );
    }
    // A key name no shortcut may hold, or a panel no key may be scoped to.
    for (from, to) in [
        ("\"key\": \"K\"", "\"key\": \"K+\""),
        ("\"key\": \"1\"", "\"key\": \"\""),
        ("\"scope\": \"cues\"", "\"scope\": \"timeline\""),
        // A pop-out window of no panel.
        ("\"scope\": \"cues\"", "\"scope\": \"timeline-window\""),
        // A tool window's name alone is no panel (schema 82).
        ("\"scope\": \"cues\"", "\"scope\": \"color\""),
    ] {
        let bad = text.replacen(from, to, 1);
        assert!(
            matches!(
                decode_shortcut_set_file(&bad),
                Err(PresetFileError::Invalid(_))
            ),
            "{to}"
        );
    }
    // A mouse button may be either stroke of a chord (schema 80).
    let mouse_chord = text
        .replacen("\"key\": \"K\"", "\"key\": \"MouseBack\"", 1)
        .replacen("\"key\": \"1\"", "\"key\": \"MouseMiddle\"", 1);
    assert_ne!(mouse_chord, text);
    assert!(decode_shortcut_set_file(&mouse_chord).is_ok());
    // A panel's pop-out window is a scope (schema 75).
    let window = text.replacen("\"scope\": \"cues\"", "\"scope\": \"cues-window\"", 1);
    assert!(decode_shortcut_set_file(&window).is_ok());
    // So is a tool window: the Colour window, the Animation editor, and the
    // scene Designer (schema 82), and it round-trips exactly.
    for tool in ["color-window", "animation-window", "designer-window"] {
        let window = text.replacen("\"scope\": \"cues\"", &format!("\"scope\": \"{tool}\""), 1);
        let decoded = decode_shortcut_set_file(&window).unwrap();
        assert_eq!(
            encode_shortcut_set_file(&decoded).unwrap(),
            window,
            "{tool}"
        );
    }
    // A second stroke is a plain stroke.
    let nested = text.replacen(
        "\"alt\": false}, \"scope\": \"cues\"",
        "\"alt\": false, \"then\": null}, \"scope\": \"cues\"",
        1,
    );
    assert!(matches!(
        decode_shortcut_set_file(&nested),
        Err(PresetFileError::Malformed { .. })
    ));
    // Names, keys, and OSC names unique within the set.
    let mut twice = sample();
    twice.shortcuts[1].name = "Intro".to_owned();
    assert!(matches!(
        encode_shortcut_set_file(&twice),
        Err(PresetFileError::Invalid(message)) if message.contains("used twice")
    ));
    let mut shared_key = sample();
    // Ctrl+K alone clashes with the Ctrl+K chord.
    shared_key.shortcuts[0].key = Some(WireShortcutKey::stroke("K", true, false, false));
    assert!(matches!(
        encode_shortcut_set_file(&shared_key),
        Err(PresetFileError::Invalid(message)) if message.contains("share a key")
    ));
    let mut shared_osc = sample();
    shared_osc.shortcuts[1].osc = Some("intro".to_owned());
    assert!(matches!(
        encode_shortcut_set_file(&shared_osc),
        Err(PresetFileError::Invalid(message)) if message.contains("OSC")
    ));
    // 1 to 64 shortcuts.
    let mut empty = sample();
    empty.shortcuts.clear();
    assert!(encode_shortcut_set_file(&empty).is_err());
    let mut many = sample();
    many.shortcuts = (0..=MAX_SHORTCUTS)
        .map(|index| spec(&format!("S{index}"), None, None))
        .collect();
    assert!(encode_shortcut_set_file(&many).is_err());
    many.shortcuts.pop();
    let text = encode_shortcut_set_file(&many).unwrap();
    assert_eq!(decode_shortcut_set_file(&text).unwrap(), many);
}

#[test]
fn an_import_adds_new_names_replaces_known_ones_and_skips_identical_ones() {
    let existing = vec![
        status(3, spec("Intro", Some(key("F9", false)), Some("intro"))),
        status(8, spec("Outro", Some(key("F10", false)), None)),
    ];
    let imported = vec![
        // Identical: left alone.
        spec("Intro", Some(key("F9", false)), Some("intro")),
        // Known name, new key: redefined under its id.
        spec("Outro", Some(key("F11", false)), None),
        // New names: fresh ids after the largest.
        spec("Break", Some(key("B", true)), Some("break")),
        spec("Scores", None, None),
    ];
    let plan = plan_shortcut_import(&existing, &imported).unwrap();
    assert_eq!((plan.added, plan.replaced, plan.unchanged), (2, 1, 1));
    assert_eq!(
        plan.sets,
        vec![
            status(8, imported[1].clone()),
            status(9, imported[2].clone()),
            status(10, imported[3].clone()),
        ]
    );
}

#[test]
fn an_import_that_would_clash_or_overflow_imports_nothing() {
    let existing = vec![
        status(1, spec("Intro", Some(key("F9", false)), Some("intro"))),
        status(2, spec("Outro", Some(key("F10", false)), Some("outro"))),
    ];
    assert_eq!(
        plan_shortcut_import(&existing, &[spec("Break", Some(key("F9", false)), None)]),
        Err(ShortcutImportError::KeyTaken {
            imported: "Break".to_owned(),
            existing: "Intro".to_owned(),
        })
    );
    assert_eq!(
        plan_shortcut_import(&existing, &[spec("Break", None, Some("outro"))]),
        Err(ShortcutImportError::OscTaken {
            imported: "Break".to_owned(),
            existing: "Outro".to_owned(),
        })
    );
    // A key the set moves away from the project shortcut that held it is
    // free again: Intro gives F9 up as Break takes it.
    let plan = plan_shortcut_import(
        &existing,
        &[
            spec("Break", Some(key("F9", false)), None),
            spec("Intro", Some(key("F1", false)), Some("intro")),
        ],
    )
    .unwrap();
    assert_eq!(
        plan.sets.iter().map(|set| set.id).collect::<Vec<_>>(),
        [1, 3],
        "Intro gives F9 up before Break takes it"
    );
    // Two project shortcuts trading keys cannot be reached one edit at a
    // time.
    assert!(matches!(
        plan_shortcut_import(
            &existing,
            &[
                spec("Intro", Some(key("F10", false)), Some("intro")),
                spec("Outro", Some(key("F9", false)), Some("outro")),
            ],
        ),
        Err(ShortcutImportError::Swap { .. })
    ));
    let full: Vec<_> = (1..=MAX_SHORTCUTS as u64)
        .map(|id| status(id, spec(&format!("S{id}"), None, None)))
        .collect();
    assert_eq!(
        plan_shortcut_import(&full, &[spec("One more", None, None)]),
        Err(ShortcutImportError::TooMany {
            total: MAX_SHORTCUTS + 1
        })
    );
    // Redefining keeps the count.
    assert!(plan_shortcut_import(&full, &[spec("S1", Some(key("Q", false)), None)]).is_ok());
}

#[test]
fn an_import_after_the_largest_id_takes_the_lowest_free_one() {
    let existing = vec![status(u64::MAX, spec("Last", None, None))];
    let plan = plan_shortcut_import(&existing, &[spec("New", None, None)]).unwrap();
    assert_eq!(plan.sets[0].id, 1);
}

/// Chords that end on different strokes, and one key in different panels,
/// import side by side; a chord's first stroke alone or the same key in an
/// overlapping scope does not.
#[test]
fn chords_and_scoped_keys_clash_only_where_they_overlap() {
    let chord = |then: &str, scope: Option<&str>| WireShortcutKey {
        then: Some(Box::new(key(then, false))),
        scope: scope.map(str::to_owned),
        ..WireShortcutKey::stroke("K", true, false, false)
    };
    let scoped = |name: &str, scope: &str| WireShortcutKey {
        scope: Some(scope.to_owned()),
        ..key(name, false)
    };
    let existing = vec![
        status(1, spec("One", Some(chord("1", None)), None)),
        status(2, spec("Mute", Some(scoped("M", "mixer")), None)),
    ];
    let plan = plan_shortcut_import(
        &existing,
        &[
            spec("Two", Some(chord("2", None)), None),
            spec("Mark", Some(scoped("M", "cues")), None),
        ],
    )
    .unwrap();
    assert_eq!(plan.added, 2);
    for clash in [
        WireShortcutKey::stroke("K", true, false, false),
        chord("1", Some("cues")),
        key("M", false),
        scoped("M", "mixer"),
    ] {
        assert!(
            matches!(
                plan_shortcut_import(&existing, &[spec("New", Some(clash.clone()), None)]),
                Err(ShortcutImportError::KeyTaken { .. })
            ),
            "{clash:?}"
        );
    }
}
