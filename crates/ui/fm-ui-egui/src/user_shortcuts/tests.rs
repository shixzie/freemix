use egui::{Event, Key, Modifiers};
use fm_protocol::{
    ShortcutSpec, WireCueAction, WireShortcutAction, WireShortcutCueOperation, WireShortcutKey,
};

use super::*;
use crate::hover_audit::{audit_shortcuts, shortcuts_fixture};

fn key(name: &str, command: bool, shift: bool, alt: bool) -> WireShortcutKey {
    WireShortcutKey {
        key: name.into(),
        command,
        shift,
        alt,
        then: None,
        scope: None,
    }
}

/// A press as egui reports it on Linux and Windows.
const fn press(command: bool, shift: bool, alt: bool) -> Modifiers {
    Modifiers {
        alt,
        ctrl: command,
        shift,
        mac_cmd: false,
        command,
    }
}

fn key_event(key: Key, modifiers: Modifiers, repeat: bool) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat,
        modifiers,
    }
}

/// Runs one frame of `events` through the user shortcut keys, optionally
/// with a text field focused; returns the intents.
fn run_frame(
    shell: &mut StudioShell,
    state: &StudioUiState,
    events: Vec<Event>,
    typing: bool,
) -> Vec<StudioIntent> {
    run_frame_on(&egui::Context::default(), shell, state, events, typing)
}

/// [`run_frame`] on `context`, which remembers the keys held down.
fn run_frame_on(
    context: &egui::Context,
    shell: &mut StudioShell,
    state: &StudioUiState,
    events: Vec<Event>,
    typing: bool,
) -> Vec<StudioIntent> {
    run_frame_in(context, shell, state, events, typing, None)
}

/// [`run_frame_on`] in `popout`'s pop-out window, or the main window.
fn run_frame_in(
    context: &egui::Context,
    shell: &mut StudioShell,
    state: &StudioUiState,
    events: Vec<Event>,
    typing: bool,
    popout: Option<crate::dock::PanelId>,
) -> Vec<StudioIntent> {
    context.begin_pass(egui::RawInput {
        events,
        ..Default::default()
    });
    if typing {
        context.memory_mut(|memory| memory.request_focus(egui::Id::new("typing")));
    }
    let mut ui = egui::Ui::new(
        context.clone(),
        egui::Id::new("studio-user-shortcuts"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(context.viewport_rect()),
    );
    let mut intents = Vec::new();
    shell.fire_user_shortcut_keys(&mut ui, state, popout, &mut intents);
    drop(ui);
    let _ = context.end_pass();
    intents
}

fn fire(shortcut: u64) -> StudioIntent {
    StudioIntent::Shortcut(ShortcutIntent::Fire {
        shortcut,
        targets: fm_protocol::WireTargetBindings::default(),
        value: None,
    })
}

fn toasts(shell: &StudioShell) -> Vec<String> {
    shell
        .toasts
        .iter()
        .map(|toast| toast.message.clone())
        .collect()
}

#[test]
fn console_keys_are_reserved_and_free_keys_are_not() {
    for (taken, keys) in [
        (key("B", false, false, false), "B"),
        (key("1", false, false, false), "1 – 8"),
        (key("R", true, true, false), crate::keymap::RECORD_SHORTCUT),
        (key("Z", true, false, false), crate::UNDO_SHORTCUT),
        // The window keys match logically: extra Shift or Alt still opens.
        (
            key("G", true, true, false),
            crate::studio_color::COLOR_SHORTCUT,
        ),
        (
            key("A", true, true, true),
            crate::studio_animation::ANIMATION_SHORTCUT,
        ),
        (key("Tab", false, true, false), "Tab"),
        (key("Escape", false, false, false), "Esc"),
    ] {
        assert_eq!(reserved_by(&taken), Some(keys), "{taken:?}");
    }
    for free in [
        key("F9", false, false, false),
        key("B", true, false, false),
        key("B", false, true, false),
        key("9", false, false, false),
        key("R", true, false, false),
        key("J", false, false, true),
    ] {
        assert_eq!(reserved_by(&free), None, "{free:?}");
    }
    assert_eq!(key_text(&key("F9", true, true, true)), "Ctrl+Shift+Alt+F9");
    assert_eq!(key_text(&key("J", false, false, true)), "Alt+J");
}

#[test]
fn a_capture_takes_the_first_new_press_and_swallows_the_rest() {
    let mut events = vec![
        key_event(Key::ShiftLeft, press(false, true, false), false),
        Event::Text("x".into()),
        key_event(Key::F9, press(true, true, false), true),
        key_event(Key::F9, press(true, true, false), false),
        key_event(Key::J, press(false, false, false), false),
        Event::PointerGone,
    ];
    assert_eq!(
        capture(&mut events),
        Some(Captured::Key(key("F9", true, true, false)))
    );
    // Only the pointer event is left for the rest of the console.
    assert_eq!(events, vec![Event::PointerGone]);

    let mut events = vec![key_event(Key::Escape, Modifiers::NONE, false)];
    assert_eq!(capture(&mut events), Some(Captured::Cancel));
    // macOS's Control alone cannot name a shortcut key.
    let control = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    let mut events = vec![key_event(Key::K, control, false)];
    assert_eq!(capture(&mut events), Some(Captured::Unsupported));
    // A key whose name is longer than a shortcut key may be.
    let mut events = vec![key_event(Key::CloseCurlyBracket, Modifiers::NONE, false)];
    assert_eq!(capture(&mut events), Some(Captured::Unsupported));
    let mut events = vec![Event::Text("x".into())];
    assert_eq!(capture(&mut events), None);
    assert_eq!(events, Vec::<Event>::new());
}

#[test]
fn a_key_fires_its_shortcut_once_and_never_behind_a_dialog_or_text_field() {
    type Open = fn(&mut StudioShell);
    let state = shortcuts_fixture(true);
    let f9 = key_event(Key::F9, Modifiers::NONE, false);
    let mut shell = StudioShell::default();
    assert_eq!(
        run_frame(&mut shell, &state, vec![f9.clone()], false),
        vec![fire(1)]
    );

    // A held key repeats without firing again.
    let context = egui::Context::default();
    assert_eq!(
        run_frame_on(&context, &mut shell, &state, vec![f9.clone()], false),
        vec![fire(1)]
    );
    for _ in 0..3 {
        let repeat = key_event(Key::F9, Modifiers::NONE, true);
        assert_eq!(
            run_frame_on(&context, &mut shell, &state, vec![repeat], false),
            Vec::new()
        );
    }
    // Other modifiers make another key.
    for modifiers in [press(false, true, false), press(true, false, false)] {
        let other = key_event(Key::F9, modifiers, false);
        assert_eq!(
            run_frame(&mut shell, &state, vec![other], false),
            Vec::new()
        );
    }

    // A focused text field keeps the key.
    assert_eq!(
        run_frame(&mut shell, &state, vec![f9.clone()], true),
        Vec::new()
    );

    // No key acts behind a modal dialog, the keyboard reference, or the
    // Shortcuts window itself.
    let dialogs: [(&str, Open); 4] = [
        ("about", |shell| shell.show_about = true),
        ("reference", |shell| shell.show_shortcuts = true),
        ("preferences", |shell| shell.show_preferences = true),
        ("shortcuts", |shell| shell.user_shortcuts.open = true),
    ];
    for (dialog, open) in dialogs {
        let mut shell = StudioShell::default();
        open(&mut shell);
        assert_eq!(
            run_frame(&mut shell, &state, vec![f9.clone()], false),
            Vec::new(),
            "{dialog}"
        );
        assert_eq!(toasts(&shell), Vec::<String>::new(), "{dialog}");
    }
}

#[test]
fn a_fire_this_session_cannot_send_is_announced_instead() {
    // A disabled shortcut's key says so.
    let state = shortcuts_fixture(true);
    let mut shell = StudioShell::default();
    let alt_j = key_event(Key::J, press(false, false, true), false);
    assert_eq!(
        run_frame(&mut shell, &state, vec![alt_j], false),
        Vec::new()
    );
    let said = toasts(&shell);
    assert_eq!(said.len(), 1);
    assert!(
        said[0].contains("Scores") && said[0].contains("disabled"),
        "{said:?}"
    );

    // A viewer cannot fire at all.
    let viewer = shortcuts_fixture(false);
    let mut shell = StudioShell::default();
    let f9 = key_event(Key::F9, Modifiers::NONE, false);
    assert_eq!(
        run_frame(&mut shell, &viewer, vec![f9.clone()], false),
        Vec::new()
    );
    assert_eq!(toasts(&shell).len(), 1);

    // A session without audio control cannot fire actions that mute.
    let mut no_audio = shortcuts_fixture(true);
    no_audio.can_control_audio = false;
    let found = &audit_shortcuts()[0];
    assert_eq!(
        fire_block(&no_audio, found),
        Some(Msg::CueReasonChainAudio.en())
    );
    let mut shell = StudioShell::default();
    assert_eq!(
        run_frame(&mut shell, &no_audio, vec![f9], false),
        Vec::new()
    );
    assert!(toasts(&shell)[0].contains("Lower third"));
}

#[test]
fn fire_blocks_name_missing_targets() {
    let state = shortcuts_fixture(true);
    let mut shortcut = audit_shortcuts()[1].clone();
    assert_eq!(fire_block(&state, &shortcut), None);
    shortcut.spec.action = WireShortcutAction::Cue {
        list: 99,
        operation: WireShortcutCueOperation::Stop,
    };
    assert_eq!(
        fire_block(&state, &shortcut),
        Some(Msg::UserShortcutReasonNoList.en())
    );
    shortcut.spec.action = WireShortcutAction::Script { script: 99 };
    assert_eq!(
        fire_block(&state, &shortcut),
        Some(Msg::UserShortcutReasonNoScript.en())
    );
    shortcut.spec.action = WireShortcutAction::Script { script: 1 };
    assert_eq!(fire_block(&state, &shortcut), None);
}

#[test]
fn an_osc_name_fires_its_shortcut_under_the_same_gates() {
    let state = shortcuts_fixture(true);
    let mut shell = StudioShell::default();
    assert_eq!(
        shell.user_shortcut_osc_intent(&state, "lower-third", None),
        Some(fire(1))
    );
    assert_eq!(
        shell.user_shortcut_osc_intent(&state, "open", None),
        Some(fire(2))
    );
    assert_eq!(
        shell.user_shortcut_osc_intent(&state, "unknown", None),
        None
    );
    assert_eq!(toasts(&shell), Vec::<String>::new());
    // RC-004: an OSC fire carries the console's selection, as a key does.
    let selected = fm_types::InputId::new(core::num::NonZeroU128::new(3).unwrap());
    shell.selected_input = Some(selected);
    assert_eq!(
        shell.user_shortcut_osc_intent(&state, "open", None),
        Some(StudioIntent::Shortcut(ShortcutIntent::Fire {
            shortcut: 2,
            targets: fm_protocol::WireTargetBindings {
                selected: Some(fm_protocol::WireInputId::from_domain(selected)),
                ..fm_protocol::WireTargetBindings::default()
            },
            value: None,
        }))
    );
    shell.selected_input = None;
    let viewer = shortcuts_fixture(false);
    assert_eq!(shell.user_shortcut_osc_intent(&viewer, "open", None), None);
    assert_eq!(toasts(&shell).len(), 1);
}

#[test]
fn save_is_held_for_clashes_waits_and_bad_names() {
    let state = shortcuts_fixture(true);
    let shortcuts = audit_shortcuts();
    let base = shortcuts[0].spec.clone();
    let block = |spec: &ShortcutSpec| save_block(&state, &shortcuts, 1, spec, true);
    assert_eq!(block(&base), None);
    assert_eq!(
        save_block(&state, &shortcuts, 1, &base, false),
        Some(Msg::UserShortcutReasonUnchanged.en())
    );
    let with = |edit: &dyn Fn(&mut ShortcutSpec)| {
        let mut spec = base.clone();
        edit(&mut spec);
        spec
    };
    for (spec, reason) in [
        (with(&|spec| spec.name = " ".into()), Msg::ScriptReasonName),
        (
            with(&|spec| spec.key = Some(key("B", false, false, false))),
            Msg::UserShortcutReasonReserved,
        ),
        (
            with(&|spec| spec.key = Some(key("J", false, false, true))),
            Msg::UserShortcutReasonKeyTaken,
        ),
        (
            with(&|spec| spec.osc = Some("Open".into())),
            Msg::UserShortcutReasonOsc,
        ),
        (
            with(&|spec| spec.osc = Some("open".into())),
            Msg::UserShortcutReasonOscTaken,
        ),
        (
            with(&|spec| spec.action = WireShortcutAction::Actions(Vec::new())),
            Msg::UserShortcutReasonNoActions,
        ),
        (
            with(&|spec| {
                spec.action =
                    WireShortcutAction::Actions(vec![WireCueAction::Delay { millis: 200 }]);
            }),
            Msg::UserShortcutReasonWaits,
        ),
        (
            with(&|spec| {
                spec.action = WireShortcutAction::Actions(vec![
                    WireCueAction::Preview {
                        input: fm_types::InputTarget::Fixed(fm_protocol::WireInputId::from_domain(
                            fm_types::InputId::new(core::num::NonZeroU128::new(2).unwrap(),)
                        ))
                    };
                    MAX_SHORTCUT_ACTIONS + 1
                ]);
            }),
            Msg::UserShortcutReasonTooManyActions,
        ),
    ] {
        assert_eq!(block(&spec), Some(reason.en()), "{spec:?}");
    }
    // The same key with Shift is another key; a shortcut keeps its own.
    assert_eq!(
        block(&with(&|spec| spec.key = Some(key("J", false, true, true)))),
        None
    );
    assert_eq!(
        save_block(&state, &shortcuts, 3, &shortcuts[2].spec, true),
        None
    );
    // A viewer cannot save at all.
    assert!(save_block(&shortcuts_fixture(false), &shortcuts, 1, &base, true).is_some());
}

#[test]
fn the_draft_follows_the_project_until_edited() {
    let mut shortcuts = audit_shortcuts();
    let mut window = ShortcutsWindow::default();
    window.follow(&shortcuts);
    assert_eq!(window.selected, Some(1));
    assert!(!window.dirty());

    // An unchanged draft follows an edit from elsewhere.
    shortcuts[0].spec.name = "Renamed".into();
    window.follow(&shortcuts);
    assert_eq!(window.draft.as_ref().unwrap().name, "Renamed");

    // An edited draft keeps the operator's text; its saved echo lands.
    window.draft.as_mut().unwrap().name = "Mine".into();
    shortcuts[0].spec.enabled = false;
    window.follow(&shortcuts);
    assert_eq!(window.draft.as_ref().unwrap().name, "Mine");
    assert!(window.dirty());
    shortcuts[0].spec = window.draft.clone().unwrap();
    window.follow(&shortcuts);
    assert!(!window.dirty());

    // A removed shortcut moves the draft to the first one left.
    shortcuts.remove(0);
    window.follow(&shortcuts);
    assert_eq!(window.selected, Some(2));

    // A new draft takes the next free id and is dirty until saved.
    window.new_draft(Locale::ENGLISH, &shortcuts);
    assert_eq!(window.selected, Some(4));
    assert!(window.dirty());
}

/// Shortcut ids are any nonzero u64 another client chose: a project
/// holding id `u64::MAX` gives a new draft the lowest free id instead of
/// overflowing.
#[test]
fn a_new_draft_after_the_largest_id_takes_the_lowest_free_one() {
    let mut shortcuts = audit_shortcuts();
    shortcuts[0].id = u64::MAX;
    let mut window = ShortcutsWindow::default();
    window.new_draft(Locale::ENGLISH, &shortcuts);
    let taken: Vec<u64> = shortcuts.iter().map(|found| found.id).collect();
    let id = window.selected.unwrap();
    assert_ne!(id, 0);
    assert!(!taken.contains(&id), "{id} is taken: {taken:?}");
    assert_eq!(id, 1);
}

#[test]
fn the_keyboard_reference_lists_project_shortcuts() {
    let state = shortcuts_fixture(true);
    let rows = reference_rows(Locale::ENGLISH, &state, &ShortcutKeyOverrides::default());
    assert_eq!(
        rows,
        vec![
            (
                "F9".to_owned(),
                "Lower third  (/freemix/user/lower-third)".to_owned()
            ),
            (
                "OSC only".to_owned(),
                "Open show  (/freemix/user/open)".to_owned()
            ),
            ("Alt+J".to_owned(), "Scores (off)".to_owned()),
        ]
    );
    let spanish = reference_rows(
        Locale::new(fm_ui_model::i18n::Language::Spanish),
        &state,
        &ShortcutKeyOverrides::default(),
    );
    assert_eq!(spanish[1].0, "Solo OSC");
    assert_eq!(spanish[2].1, "Scores (desactivado)");
}

/// The window sends nothing by itself, and Set key binds the next press,
/// which the console never sees.
#[test]
fn set_key_binds_the_next_press() {
    let state = shortcuts_fixture(true);
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    shell.open_user_shortcuts(&state);
    assert_eq!(shell.user_shortcuts.selected, Some(1));
    // Nothing is sent while the draft is unchanged and nothing is clicked.
    context.begin_pass(egui::RawInput::default());
    let mut intents = Vec::new();
    shell.draw_user_shortcuts_window(&context, &state, &mut intents);
    let _ = context.end_pass();
    assert_eq!(intents, Vec::new());

    // Set key captures the next press, which the console never sees.
    shell.user_shortcuts.capturing = true;
    context.begin_pass(egui::RawInput {
        events: vec![key_event(Key::F10, press(false, true, false), false)],
        ..Default::default()
    });
    let mut intents = Vec::new();
    shell.draw_user_shortcuts_window(&context, &state, &mut intents);
    let _ = context.end_pass();
    assert!(!shell.user_shortcuts.capturing);
    assert_eq!(
        shell.user_shortcuts.draft.as_ref().unwrap().key,
        Some(key("F10", false, true, false))
    );
    assert!(shell.user_shortcuts.dirty());
    assert_eq!(
        save_block(
            &state,
            &audit_shortcuts(),
            1,
            shell.user_shortcuts.draft.as_ref().unwrap(),
            true
        ),
        None
    );
}

fn imported(name: &str, key: Option<WireShortcutKey>, osc: Option<&str>) -> ShortcutSpec {
    ShortcutSpec {
        name: name.into(),
        enabled: true,
        key,
        osc: osc.map(Into::into),
        action: WireShortcutAction::Script { script: 1 },
    }
}

#[test]
fn an_imported_set_becomes_one_set_per_new_or_redefined_shortcut() {
    let state = shortcuts_fixture(true);
    let mut shell = StudioShell::default();
    let request = PresetFileRequest::ImportShortcuts {
        path: "desk.fmshortcuts".into(),
    };
    let mut redefined = audit_shortcuts()[1].spec.clone();
    redefined.key = Some(key("F11", false, false, false));
    let set = vec![
        // Unchanged, redefined by name, and new.
        audit_shortcuts()[0].spec.clone(),
        redefined.clone(),
        imported(
            "Break",
            Some(key("F10", false, false, false)),
            Some("break"),
        ),
    ];
    let intents = shell.receive_preset_file(
        &state,
        &request,
        crate::PresetFileAnswer::Shortcuts(set.clone()),
    );
    assert_eq!(
        intents,
        vec![
            StudioIntent::Shortcut(ShortcutIntent::Set {
                shortcut: 2,
                spec: Box::new(redefined),
            }),
            StudioIntent::Shortcut(ShortcutIntent::Set {
                shortcut: 4,
                spec: Box::new(set[2].clone()),
            }),
        ]
    );
    let said = toasts(&shell);
    assert!(
        said[0].contains("1 new, 1 replaced, 1 unchanged"),
        "{said:?}"
    );
}

#[test]
fn an_imported_set_that_clashes_or_takes_a_console_key_imports_nothing() {
    let state = shortcuts_fixture(true);
    // Another name on shortcut 1's F9.
    assert_eq!(
        import_plan(
            &state,
            &[imported(
                "Break",
                Some(key("F9", false, false, false)),
                None
            )]
        ),
        Err(ImportRefusal::Plan(ShortcutImportError::KeyTaken {
            imported: "Break".into(),
            existing: "Lower third".into(),
        }))
    );
    // The console's own Cut key.
    assert_eq!(
        import_plan(
            &state,
            &[imported("Cut", Some(key("C", false, false, false)), None)]
        ),
        Err(ImportRefusal::Reserved {
            name: "Cut".into(),
            keys: "C",
        })
    );
    // A viewer may not edit the project.
    let viewer = shortcuts_fixture(false);
    assert!(matches!(
        import_plan(&viewer, &[imported("Break", None, None)]),
        Err(ImportRefusal::Blocked(_))
    ));
    // A refused import is toasted and sends nothing.
    let mut shell = StudioShell::default();
    let request = PresetFileRequest::ImportShortcuts {
        path: "desk.fmshortcuts".into(),
    };
    let intents = shell.receive_preset_file(
        &state,
        &request,
        crate::PresetFileAnswer::Shortcuts(vec![imported("Break", None, Some("lower-third"))]),
    );
    assert_eq!(intents, Vec::new());
    let said = toasts(&shell);
    assert!(
        said[0].contains("Break") && said[0].contains("OSC name of Lower third"),
        "{said:?}"
    );
    // Every refusal reads in Spanish too.
    let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
    for refusal in [
        ImportRefusal::Plan(ShortcutImportError::NoFreeId),
        ImportRefusal::Plan(ShortcutImportError::TooMany { total: 65 }),
        ImportRefusal::Plan(ShortcutImportError::Swap {
            first: "A".into(),
            second: "B".into(),
        }),
        ImportRefusal::Reserved {
            name: "Cut".into(),
            keys: "C",
        },
    ] {
        assert_ne!(
            refusal.text(spanish),
            refusal.text(Locale::ENGLISH),
            "{refusal:?}"
        );
    }
}

#[test]
fn the_set_file_row_exports_what_is_saved_and_imports_only_with_edit_rights() {
    let state = shortcuts_fixture(true);
    let shortcuts = audit_shortcuts();
    assert_eq!(
        export_block(" ", &shortcuts),
        Some(Msg::AnimReasonNoPath.en())
    );
    assert_eq!(
        export_block("desk.fmshortcuts", &[]),
        Some(Msg::UserShortcutReasonNoShortcuts.en())
    );
    assert_eq!(export_block("desk.fmshortcuts", &shortcuts), None);
    assert_eq!(import_block(&state, "desk.fmshortcuts"), None);
    assert_eq!(import_block(&state, ""), Some(Msg::AnimReasonNoPath.en()));
    let viewer = shortcuts_fixture(false);
    assert!(import_block(&viewer, "desk.fmshortcuts").is_some());
    // A viewer may still export.
    assert_eq!(export_block("desk.fmshortcuts", replicated(&viewer)), None);

    let mut shell = StudioShell::default();
    shell.user_shortcuts.file_path = " desk.fmshortcuts ".into();
    shell.user_shortcuts.export_set(&state, &shortcuts);
    shell.user_shortcuts.import_set();
    let show = state.project_state.as_ref().unwrap().show_name().to_owned();
    assert_eq!(
        shell.take_preset_file_requests(),
        vec![
            PresetFileRequest::ExportShortcuts {
                path: "desk.fmshortcuts".into(),
                name: if show.trim().is_empty() {
                    "Shortcuts".into()
                } else {
                    show
                },
                shortcuts: shortcuts.into_iter().map(|found| found.spec).collect(),
            },
            PresetFileRequest::ImportShortcuts {
                path: "desk.fmshortcuts".into(),
            },
        ]
    );
    assert_eq!(shell.take_preset_file_requests(), Vec::new());
}

fn project() -> fm_types::ProjectId {
    fm_types::ProjectId::new(core::num::NonZeroU128::new(1).unwrap())
}

/// A console key replaces the project key on this console only, takes
/// the key from a shortcut whose project key it is, and "no key here"
/// silences a shortcut here.
#[test]
fn console_keys_replace_project_keys_on_this_console() {
    let state = shortcuts_fixture(true);
    let mut shell = StudioShell::default();
    let f2 = key_event(Key::F2, press(true, false, false), false);
    let f9 = key_event(Key::F9, Modifiers::NONE, false);
    let alt_j = key_event(Key::J, press(false, false, true), false);
    assert!(
        shell
            .shortcut_keys
            .set(project(), 1, Some(key("F2", true, false, false)))
    );
    assert_eq!(
        run_frame(&mut shell, &state, vec![f2], false),
        vec![fire(1)]
    );
    assert_eq!(
        run_frame(&mut shell, &state, vec![f9.clone()], false),
        Vec::new(),
        "the project key no longer fires here"
    );

    // Shortcut 2 (OSC only) takes F9 here; shortcut 1 keeps Ctrl+F2.
    assert!(
        shell
            .shortcut_keys
            .set(project(), 2, Some(key("F9", false, false, false)))
    );
    assert_eq!(
        run_frame(&mut shell, &state, vec![f9], false),
        vec![fire(2)]
    );

    // No key here: Alt+J no longer reaches shortcut 3.
    assert!(shell.shortcut_keys.set(project(), 3, None));
    assert_eq!(
        run_frame(&mut shell, &state, vec![alt_j.clone()], false),
        Vec::new()
    );
    assert_eq!(toasts(&shell), Vec::<String>::new());
    shell.shortcut_keys.clear(project(), 3);
    // Shortcut 3 is disabled, so its key is swallowed and announced.
    assert_eq!(
        run_frame(&mut shell, &state, vec![alt_j], false),
        Vec::new()
    );
    assert_eq!(toasts(&shell).len(), 1);

    // Another project's overrides do not apply.
    let mut other = StudioShell::default();
    let elsewhere = fm_types::ProjectId::new(core::num::NonZeroU128::new(2).unwrap());
    assert!(other.shortcut_keys.set(elsewhere, 1, None));
    let f9 = key_event(Key::F9, Modifiers::NONE, false);
    assert_eq!(
        run_frame(&mut other, &state, vec![f9], false),
        vec![fire(1)]
    );
}

#[test]
fn a_console_key_capture_refuses_console_keys_and_keys_taken_here() {
    let state = shortcuts_fixture(true);
    let mut overrides = ShortcutKeyOverrides::default();
    let keys = ConsoleKeys::of(&state, &overrides);
    assert_eq!(local_capture(keys, 3, Captured::Cancel), None);
    assert_eq!(
        local_capture(keys, 3, Captured::Unsupported),
        Some(Err(LocalRefusal::Unsupported))
    );
    assert_eq!(
        local_capture(keys, 3, Captured::Key(key("B", false, false, false))),
        Some(Err(LocalRefusal::Reserved("B")))
    );
    assert_eq!(
        local_capture(keys, 3, Captured::Key(key("F9", false, false, false))),
        Some(Err(LocalRefusal::Taken("Lower third".into())))
    );
    // Its own project key is fine.
    assert_eq!(
        local_capture(keys, 1, Captured::Key(key("F9", false, false, false))),
        Some(Ok(key("F9", false, false, false)))
    );
    // Once Lower third answers Ctrl+F2 here, F9 is free and Ctrl+F2 taken.
    overrides.set(project(), 1, Some(key("F2", true, false, false)));
    let keys = ConsoleKeys::of(&state, &overrides);
    assert_eq!(
        local_capture(keys, 3, Captured::Key(key("F9", false, false, false))),
        Some(Ok(key("F9", false, false, false)))
    );
    assert_eq!(
        local_capture(keys, 3, Captured::Key(key("F2", true, false, false))),
        Some(Err(LocalRefusal::Taken("Lower third".into())))
    );
}

/// "Set console key" binds the next press in this console's preferences
/// and sends nothing; the list and the keyboard reference show it.
#[test]
fn set_console_key_binds_the_next_press_locally() {
    let state = shortcuts_fixture(false);
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    shell.open_user_shortcuts(&state);
    shell.user_shortcuts.capturing_local = true;
    context.begin_pass(egui::RawInput {
        events: vec![key_event(Key::F10, press(false, true, false), false)],
        ..Default::default()
    });
    let mut intents = Vec::new();
    shell.draw_user_shortcuts_window(&context, &state, &mut intents);
    let _ = context.end_pass();
    assert_eq!(intents, Vec::new(), "a viewer may set a console key");
    assert!(!shell.user_shortcuts.capturing_local);
    assert_eq!(shell.user_shortcuts.local_refusal, None);
    assert_eq!(
        shell.shortcut_key_overrides().get(project(), 1),
        Some(&Some(key("F10", false, true, false)))
    );
    assert!(
        !shell.user_shortcuts.dirty(),
        "the project draft is untouched"
    );

    let rows = reference_rows(Locale::ENGLISH, &state, shell.shortcut_key_overrides());
    assert_eq!(rows[0].0, "Shift+F10 (this console)");
    let spanish = reference_rows(
        Locale::new(fm_ui_model::i18n::Language::Spanish),
        &state,
        shell.shortcut_key_overrides(),
    );
    assert_ne!(spanish[0].0, rows[0].0);
    let keys = ConsoleKeys::of(&state, shell.shortcut_key_overrides());
    assert_eq!(
        row_text(Locale::ENGLISH, keys, &audit_shortcuts()[0]),
        "Lower third  Shift+F10 (this console)"
    );

    // A console key is refused and reported, binding nothing.
    shell.user_shortcuts.select(&audit_shortcuts(), Some(3));
    shell.user_shortcuts.capturing_local = true;
    context.begin_pass(egui::RawInput {
        events: vec![key_event(Key::B, Modifiers::NONE, false)],
        ..Default::default()
    });
    shell.draw_user_shortcuts_window(&context, &state, &mut intents);
    let _ = context.end_pass();
    assert_eq!(
        shell.user_shortcuts.local_refusal,
        Some(LocalRefusal::Reserved("B"))
    );
    assert_eq!(shell.shortcut_key_overrides().get(project(), 3), None);
}

fn bound(id: u64, key: WireShortcutKey) -> ShortcutStatus {
    ShortcutStatus {
        id,
        spec: ShortcutSpec {
            name: format!("S{id}"),
            enabled: true,
            key: Some(key),
            osc: None,
            action: WireShortcutAction::Script { script: 1 },
        },
    }
}

fn chord_key(first: WireShortcutKey, then: WireShortcutKey) -> WireShortcutKey {
    WireShortcutKey {
        then: Some(Box::new(then)),
        ..first
    }
}

fn scoped(key: WireShortcutKey, panel: &str) -> WireShortcutKey {
    WireShortcutKey {
        scope: Some(panel.into()),
        ..key
    }
}

/// Ctrl+K then 1 fires its shortcut; Ctrl+K alone only waits, the next
/// key ends the wait whatever it is, and a wait past the timeout lapses.
#[test]
fn a_chord_fires_on_its_second_key_and_lapses() {
    let shortcuts = vec![
        bound(
            1,
            chord_key(key("K", true, false, false), key("1", false, false, false)),
        ),
        bound(
            2,
            chord_key(key("K", true, false, false), key("2", false, false, false)),
        ),
        bound(3, key("F9", false, false, false)),
    ];
    let overrides = ShortcutKeyOverrides::default();
    let keys = ConsoleKeys {
        shortcuts: &shortcuts,
        overrides: &overrides,
        project: None,
    };
    let ctrl_k = key_event(Key::K, press(true, false, false), false);
    let one = key_event(Key::Num1, Modifiers::NONE, false);
    let two = key_event(Key::Num2, Modifiers::NONE, false);
    let shift = key_event(Key::ShiftLeft, press(false, true, false), false);
    let mut chord = None;

    // Ctrl+K waits (and is swallowed); a modifier alone keeps waiting.
    let mut events = vec![ctrl_k.clone(), shift.clone()];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 0.0),
        Vec::<u64>::new()
    );
    assert_eq!(events, vec![shift]);
    assert_eq!(
        chord.as_ref().map(|pending| key_text(&pending.first)),
        Some("Ctrl+K".to_owned())
    );
    // Then 2 fires the second chord: "2" is a console key alone, but not
    // after Ctrl+K.
    let mut events = vec![two.clone()];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 1.0),
        vec![2]
    );
    assert_eq!(events, Vec::new());
    assert_eq!(chord, None);

    // Both strokes in one frame.
    let mut events = vec![ctrl_k.clone(), one.clone()];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 3.0),
        vec![1]
    );

    // A key no chord ends with ends the wait, swallowed, firing nothing.
    let f9 = key_event(Key::F9, Modifiers::NONE, false);
    let mut events = vec![ctrl_k.clone(), f9.clone()];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 4.0),
        Vec::<u64>::new()
    );
    assert_eq!(events, Vec::new());
    assert_eq!(chord, None);

    // Past the timeout the wait lapses and the key is the console's again.
    let mut events = vec![ctrl_k];
    take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 5.0);
    assert!(chord.is_some());
    let mut events = vec![one.clone()];
    assert_eq!(
        take_presses(
            &mut events,
            keys,
            KeyFocus::default(),
            &mut chord,
            5.0 + CHORD_TIMEOUT + 0.1
        ),
        Vec::<u64>::new()
    );
    assert_eq!(events, vec![one]);
    assert_eq!(chord, None);
    // A single key still fires at once.
    let mut events = vec![f9];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 9.0),
        vec![3]
    );
}

/// While a chord waits, a held key's repeats (of its first stroke or of
/// a key held from before) neither end nor cancel it; Escape, like any
/// other new press no chord ends with, cancels it, swallowed.
#[test]
fn a_waiting_chord_ignores_repeats_and_escape_cancels_it() {
    let shortcuts = vec![bound(
        1,
        chord_key(key("K", true, false, false), key("1", false, false, false)),
    )];
    let overrides = ShortcutKeyOverrides::default();
    let keys = ConsoleKeys {
        shortcuts: &shortcuts,
        overrides: &overrides,
        project: None,
    };
    let ctrl = press(true, false, false);
    let mut chord = None;
    let take = |events: Vec<Event>, chord: &mut Option<PendingChord>| {
        let mut events = events;
        let fired = take_presses(&mut events, keys, KeyFocus::default(), chord, 0.5);
        (fired, events)
    };
    let (fired, _) = take(vec![key_event(Key::K, ctrl, false)], &mut chord);
    assert!(fired.is_empty() && chord.is_some());
    // Repeats of Ctrl+K and of a held arrow key keep the wait (swallowed).
    let (fired, left) = take(
        vec![
            key_event(Key::K, ctrl, true),
            key_event(Key::ArrowDown, Modifiers::NONE, true),
        ],
        &mut chord,
    );
    assert!(fired.is_empty() && left.is_empty());
    assert!(chord.is_some());
    // The second stroke still completes the chord.
    let (fired, _) = take(
        vec![key_event(Key::Num1, Modifiers::NONE, false)],
        &mut chord,
    );
    assert_eq!(fired, vec![1]);
    assert_eq!(chord, None);
    // Escape cancels a wait: swallowed, nothing fires.
    take(vec![key_event(Key::K, ctrl, false)], &mut chord);
    let (fired, left) = take(
        vec![key_event(Key::Escape, Modifiers::NONE, false)],
        &mut chord,
    );
    assert!(fired.is_empty() && left.is_empty());
    assert_eq!(chord, None);
}

/// A text field (or dialog) taking the keyboard ends a waiting chord, so
/// the first key after it is not swallowed as the chord's second stroke.
#[test]
fn a_text_field_taking_focus_ends_a_waiting_chord() {
    let state = crate::hover_audit::shortcuts_fixture_with(vec![bound(
        4,
        chord_key(key("K", true, false, false), key("1", false, false, false)),
    )]);
    // The fixture binds F9 to shortcut 1.
    let mut shell = StudioShell::default();
    let ctrl_k = key_event(Key::K, press(true, false, false), false);
    let f9 = key_event(Key::F9, Modifiers::NONE, false);
    assert_eq!(
        run_frame(&mut shell, &state, vec![ctrl_k], false),
        Vec::new()
    );
    assert!(shell.chord.is_some());
    // A field takes focus within the chord's timeout.
    run_frame(&mut shell, &state, Vec::new(), true);
    assert_eq!(shell.chord, None);
    // Back on the console, F9 is F9 again rather than a chord's end.
    assert_eq!(
        run_frame(&mut shell, &state, vec![f9], false),
        vec![fire(1)]
    );
}

/// Runs one main-window frame of `events` through the console keys and
/// then the project's shortcut keys, as the console draws them.
fn run_console_frame(
    context: &egui::Context,
    shell: &mut StudioShell,
    state: &StudioUiState,
    events: Vec<Event>,
    time: f64,
) -> Vec<StudioIntent> {
    context.begin_pass(egui::RawInput {
        events,
        time: Some(time),
        ..Default::default()
    });
    let mut ui = egui::Ui::new(
        context.clone(),
        egui::Id::new("studio-console-keys"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(context.viewport_rect()),
    );
    let manual = crate::manual_transition_availability(
        crate::ManualTransitionGate::from_state(state),
        false,
    );
    let mut intents = Vec::new();
    shell.console_and_user_keys(&mut ui, state, None, manual, &mut intents);
    drop(ui);
    let _ = context.end_pass();
    intents
}

/// A chord's second stroke is the chord's even when it is a console key
/// alone (C cuts, 1 selects a preview): it fires the shortcut, never the
/// console's action, and the wait still lapses back to the console.
#[test]
fn a_chord_takes_its_second_stroke_before_the_console_keys() {
    let state = crate::hover_audit::shortcuts_fixture_with(vec![
        bound(
            4,
            chord_key(key("K", true, false, false), key("C", false, false, false)),
        ),
        bound(
            5,
            chord_key(key("K", true, false, false), key("1", false, false, false)),
        ),
    ]);
    let mut shell = StudioShell::default();
    let ctrl_k = key_event(Key::K, press(true, false, false), false);
    let c = key_event(Key::C, Modifiers::NONE, false);
    let one = key_event(Key::Num1, Modifiers::NONE, false);
    // A fresh context each frame: no key is still held from the last.
    let mut frame = |events, time| {
        run_console_frame(&egui::Context::default(), &mut shell, &state, events, time)
    };

    // The console's own keys act alone.
    assert_eq!(frame(vec![c.clone()], 0.0), vec![StudioIntent::Cut]);
    assert!(matches!(
        frame(vec![one.clone()], 0.1).as_slice(),
        [StudioIntent::SelectPreview(_)]
    ));

    assert_eq!(frame(vec![ctrl_k.clone()], 1.0), Vec::new());
    assert_eq!(frame(vec![c.clone()], 1.5), vec![fire(4)]);
    assert_eq!(frame(vec![ctrl_k.clone()], 2.0), Vec::new());
    assert_eq!(frame(vec![one.clone()], 2.5), vec![fire(5)]);
    // The chord is over: C cuts again.
    assert_eq!(frame(vec![c.clone()], 3.0), vec![StudioIntent::Cut]);

    // A lapsed wait leaves the key to the console.
    assert_eq!(frame(vec![ctrl_k], 4.0), Vec::new());
    assert_eq!(
        frame(vec![c], 4.0 + CHORD_TIMEOUT + 0.1),
        vec![StudioIntent::Cut]
    );
}

/// A key scoped to a panel answers only while that panel has the focus,
/// so two panels may give one key to different shortcuts.
#[test]
fn a_scoped_key_answers_only_in_its_panel() {
    let shortcuts = vec![
        bound(1, scoped(key("M", false, false, false), "mixer")),
        bound(2, scoped(key("M", false, false, false), "cues")),
        bound(3, key("F9", false, false, false)),
    ];
    let overrides = ShortcutKeyOverrides::default();
    let keys = ConsoleKeys {
        shortcuts: &shortcuts,
        overrides: &overrides,
        project: None,
    };
    let m = key_event(Key::M, Modifiers::NONE, false);
    let f9 = key_event(Key::F9, Modifiers::NONE, false);
    let mut chord = None;
    for (panel, fired) in [
        (Some("mixer"), vec![1, 3]),
        (Some("cues"), vec![2, 3]),
        (Some("titles"), vec![3]),
        (None, vec![3]),
    ] {
        let mut events = vec![m.clone(), f9.clone()];
        assert_eq!(
            take_presses(
                &mut events,
                keys,
                KeyFocus {
                    panel,
                    window: None
                },
                &mut chord,
                0.0
            ),
            fired,
            "{panel:?}"
        );
        // An unanswered M stays for the console.
        assert_eq!(events.len(), 2 - fired.len(), "{panel:?}");
    }
}

#[test]
fn key_scopes_name_the_console_panels() {
    assert_eq!(
        crate::dock::PanelId::ALL.map(crate::dock::PanelId::key),
        fm_protocol::SHORTCUT_SCOPES
    );
    let key = scoped(
        chord_key(key("K", true, false, false), key("1", false, false, false)),
        "cues",
    );
    assert_eq!(key_text(&key), "Ctrl+K, 1");
    let words = key_words(Locale::ENGLISH, &key, false);
    assert!(words.starts_with("Ctrl+K, 1 in "), "{words}");
    let spanish = key_words(
        Locale::new(fm_ui_model::i18n::Language::Spanish),
        &key,
        false,
    );
    assert!(spanish.contains(" en "), "{spanish}");
}

/// Add second key captures the next press as the chord's second stroke;
/// a new first stroke keeps the second and the scope.
#[test]
fn add_second_key_makes_the_key_a_chord() {
    let state = shortcuts_fixture(true);
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    shell.open_user_shortcuts(&state);
    let frame = |shell: &mut StudioShell, event: Event| {
        context.begin_pass(egui::RawInput {
            events: vec![event],
            ..Default::default()
        });
        let mut intents = Vec::new();
        shell.draw_user_shortcuts_window(&context, &state, &mut intents);
        let _ = context.end_pass();
        intents
    };
    shell.user_shortcuts.capturing = true;
    shell.user_shortcuts.capturing_then = true;
    assert_eq!(
        frame(&mut shell, key_event(Key::Num1, Modifiers::NONE, false)),
        Vec::new()
    );
    let draft = shell.user_shortcuts.draft.clone().unwrap();
    assert_eq!(key_text(draft.key.as_ref().unwrap()), "F9, 1");
    shell
        .user_shortcuts
        .draft
        .as_mut()
        .unwrap()
        .key
        .as_mut()
        .unwrap()
        .scope = Some("cues".into());
    shell.user_shortcuts.capturing = true;
    shell.user_shortcuts.capturing_then = false;
    frame(
        &mut shell,
        key_event(Key::K, press(true, false, false), false),
    );
    let key = shell.user_shortcuts.draft.clone().unwrap().key.unwrap();
    assert_eq!(key_text(&key), "Ctrl+K, 1");
    assert_eq!(key.scope.as_deref(), Some("cues"));
    assert_eq!(
        save_block(
            &state,
            &audit_shortcuts(),
            1,
            shell.user_shortcuts.draft.as_ref().unwrap(),
            true
        ),
        None
    );
}

fn mouse_event(button: egui::PointerButton, modifiers: Modifiers) -> Event {
    Event::PointerButton {
        pos: egui::pos2(10.0, 10.0),
        button,
        pressed: true,
        modifiers,
    }
}

fn windowed(key: WireShortcutKey, panel: &str) -> WireShortcutKey {
    scoped(key, &format!("{panel}-window"))
}

/// Set key takes the middle, back, and forward buttons with their
/// modifiers; the primary and secondary buttons stay the console's.
#[test]
fn a_capture_takes_a_mouse_button_with_its_modifiers() {
    let primary = mouse_event(egui::PointerButton::Primary, Modifiers::NONE);
    let mut events = vec![
        primary.clone(),
        mouse_event(egui::PointerButton::Extra1, press(true, false, true)),
        mouse_event(egui::PointerButton::Middle, Modifiers::NONE),
    ];
    assert_eq!(
        capture(&mut events),
        Some(Captured::Key(key("MouseBack", true, false, true)))
    );
    assert_eq!(events, vec![primary]);
    for (button, name) in [
        (egui::PointerButton::Middle, "MouseMiddle"),
        (egui::PointerButton::Extra2, "MouseForward"),
    ] {
        let mut events = vec![mouse_event(button, Modifiers::NONE)];
        assert_eq!(
            capture(&mut events),
            Some(Captured::Key(key(name, false, false, false)))
        );
    }
    // macOS's Control has no place in a shortcut, with a button either.
    let control = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    let mut events = vec![mouse_event(egui::PointerButton::Middle, control)];
    assert_eq!(capture(&mut events), Some(Captured::Unsupported));
    let mut events = vec![mouse_event(egui::PointerButton::Secondary, Modifiers::NONE)];
    assert_eq!(capture(&mut events), None);
    assert_eq!(events.len(), 1);
}

/// A mouse button fires its shortcut at once with exactly its modifiers
/// and takes only its own press; as a stroke like a key it ends a waiting
/// chord, firing only the chord it completes; the primary button stays the
/// console's.
#[test]
fn a_mouse_button_fires_its_shortcut_with_exactly_its_modifiers() {
    let shortcuts = vec![
        bound(1, key("MouseBack", false, false, false)),
        bound(2, key("MouseBack", false, true, false)),
        bound(
            3,
            chord_key(key("K", true, false, false), key("1", false, false, false)),
        ),
    ];
    let overrides = ShortcutKeyOverrides::default();
    let keys = ConsoleKeys {
        shortcuts: &shortcuts,
        overrides: &overrides,
        project: None,
    };
    let mut chord = None;
    let back = mouse_event(egui::PointerButton::Extra1, Modifiers::NONE);
    let shift_back = mouse_event(egui::PointerButton::Extra1, press(false, true, false));
    let ctrl_back = mouse_event(egui::PointerButton::Extra1, press(true, false, false));
    let primary = mouse_event(egui::PointerButton::Primary, Modifiers::NONE);
    let release = Event::PointerButton {
        pos: egui::pos2(10.0, 10.0),
        button: egui::PointerButton::Extra1,
        pressed: false,
        modifiers: Modifiers::NONE,
    };
    let mut events = vec![
        primary.clone(),
        back,
        release.clone(),
        shift_back,
        ctrl_back.clone(),
    ];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 0.0),
        vec![1, 2]
    );
    assert_eq!(events, vec![primary, release, ctrl_back]);

    // A button no chord ends with ends the wait, swallowed, firing nothing.
    let mut events = vec![
        key_event(Key::K, press(true, false, false), false),
        mouse_event(egui::PointerButton::Extra1, Modifiers::NONE),
    ];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 1.0),
        Vec::<u64>::new()
    );
    assert_eq!(events, Vec::<Event>::new());
    assert_eq!(chord, None);
}

/// RC-004 (schema 80): a mouse button may be either stroke of a chord. A
/// chord started by a button waits for its second stroke; a key chord may
/// end on a button; a primary click never touches a waiting chord.
#[test]
fn a_mouse_button_may_start_or_end_a_chord() {
    let shortcuts = vec![
        bound(
            1,
            chord_key(
                key("MouseBack", false, false, false),
                key("1", false, false, false),
            ),
        ),
        bound(
            2,
            chord_key(
                key("K", true, false, false),
                key("MouseMiddle", false, false, false),
            ),
        ),
        bound(3, key("MouseMiddle", false, false, false)),
    ];
    let overrides = ShortcutKeyOverrides::default();
    let keys = ConsoleKeys {
        shortcuts: &shortcuts,
        overrides: &overrides,
        project: None,
    };
    let mut chord = None;
    let primary = mouse_event(egui::PointerButton::Primary, Modifiers::NONE);
    // Back starts shortcut 1's chord; the primary click stays and the
    // chord keeps waiting; 1 ends it and fires.
    let mut events = vec![
        mouse_event(egui::PointerButton::Extra1, Modifiers::NONE),
        primary.clone(),
    ];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 0.0),
        Vec::<u64>::new()
    );
    assert_eq!(events, vec![primary.clone()]);
    assert_eq!(
        chord.as_ref().map(|pending| key_text(&pending.first)),
        Some("MouseBack".to_owned())
    );
    let mut events = vec![key_event(Key::Num1, Modifiers::NONE, false)];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 1.0),
        vec![1]
    );
    assert_eq!(chord, None);

    // Ctrl+K waits; the middle button ends the chord and fires 2, not the
    // middle button's own shortcut 3.
    let mut events = vec![
        key_event(Key::K, press(true, false, false), false),
        mouse_event(egui::PointerButton::Middle, Modifiers::NONE),
    ];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 2.0),
        vec![2]
    );
    assert_eq!(events, Vec::<Event>::new());
    assert_eq!(chord, None);
    // Alone, the middle button fires 3.
    let mut events = vec![mouse_event(egui::PointerButton::Middle, Modifiers::NONE)];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 3.0),
        vec![3]
    );
}

/// A key or button scoped to a pop-out window answers only in that
/// window; a pop-out window also answers unscoped keys and its own
/// panel's.
#[test]
fn a_window_scoped_key_answers_only_in_its_pop_out_window() {
    let shortcuts = vec![
        bound(1, windowed(key("M", false, false, false), "mixer")),
        bound(2, windowed(key("M", false, false, false), "cues")),
        bound(3, scoped(key("F9", false, false, false), "mixer")),
        bound(
            4,
            windowed(key("MouseMiddle", false, false, false), "mixer"),
        ),
    ];
    let overrides = ShortcutKeyOverrides::default();
    let keys = ConsoleKeys {
        shortcuts: &shortcuts,
        overrides: &overrides,
        project: None,
    };
    let presses = || {
        vec![
            key_event(Key::M, Modifiers::NONE, false),
            key_event(Key::F9, Modifiers::NONE, false),
            mouse_event(egui::PointerButton::Middle, Modifiers::NONE),
        ]
    };
    let mut chord = None;
    for (focus, fired) in [
        (
            KeyFocus {
                panel: Some("mixer"),
                window: Some("mixer"),
            },
            vec![1, 3, 4],
        ),
        (
            KeyFocus {
                panel: Some("cues"),
                window: Some("cues"),
            },
            vec![2],
        ),
        // The main window with the Mixer panel focused.
        (
            KeyFocus {
                panel: Some("mixer"),
                window: None,
            },
            vec![3],
        ),
        (KeyFocus::default(), vec![]),
    ] {
        let mut events = presses();
        assert_eq!(
            take_presses(&mut events, keys, focus, &mut chord, 0.0),
            fired,
            "{focus:?}"
        );
        assert_eq!(events.len(), 3 - fired.len(), "{focus:?}");
    }
}

/// The shell fires a pop-out window's presses as that window's: its
/// window-scoped keys and buttons, and the console's unscoped keys.
#[test]
fn a_pop_out_window_fires_its_own_keys_and_buttons() {
    let script = |id: u64, key: WireShortcutKey| ShortcutStatus {
        spec: ShortcutSpec {
            action: WireShortcutAction::Script { script: 1 },
            ..bound(id, key).spec
        },
        id,
    };
    let state = crate::hover_audit::shortcuts_fixture_with(vec![
        script(4, windowed(key("MouseBack", false, false, false), "mixer")),
        script(5, key("MouseMiddle", true, false, false)),
        script(6, windowed(key("F10", false, false, false), "cues")),
    ]);
    let mut shell = StudioShell::default();
    let back = mouse_event(egui::PointerButton::Extra1, Modifiers::NONE);
    let ctrl_middle = mouse_event(egui::PointerButton::Middle, press(true, false, false));
    let f9 = key_event(Key::F9, Modifiers::NONE, false);
    let f10 = key_event(Key::F10, Modifiers::NONE, false);
    let mut frame = |events: Vec<Event>, popout| {
        // A fresh context each frame: no key is still held from the last.
        run_frame_in(
            &egui::Context::default(),
            &mut shell,
            &state,
            events,
            false,
            popout,
        )
    };
    assert_eq!(
        frame(vec![back.clone(), ctrl_middle.clone(), f10.clone()], None),
        vec![fire(5)]
    );
    assert_eq!(
        frame(
            vec![back.clone(), f9, f10.clone()],
            Some(crate::dock::PanelId::Mixer)
        ),
        vec![fire(4), fire(1)]
    );
    assert_eq!(
        frame(vec![back, f10], Some(crate::dock::PanelId::Cues)),
        vec![fire(6)]
    );
    // A focused text field keeps the buttons too.
    assert_eq!(
        run_frame(&mut shell, &state, vec![ctrl_middle], true),
        Vec::new()
    );
}

/// A captured mouse button may be either stroke of a chord (schema 80): as
/// the second stroke it ends the chord; as the first it keeps the scope.
#[test]
fn set_key_binds_a_mouse_button_as_either_stroke() {
    let state = shortcuts_fixture(true);
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    shell.open_user_shortcuts(&state);
    let frame = |shell: &mut StudioShell, event: Event| {
        context.begin_pass(egui::RawInput {
            events: vec![event],
            ..Default::default()
        });
        let mut intents = Vec::new();
        shell.draw_user_shortcuts_window(&context, &state, &mut intents);
        let _ = context.end_pass();
    };
    shell.user_shortcuts.draft.as_mut().unwrap().key = Some(windowed(
        chord_key(key("K", true, false, false), key("1", false, false, false)),
        "mixer",
    ));
    // A button as the second stroke ends the chord.
    shell.user_shortcuts.capturing = true;
    shell.user_shortcuts.capturing_then = true;
    frame(
        &mut shell,
        mouse_event(egui::PointerButton::Extra2, Modifiers::NONE),
    );
    assert!(!shell.user_shortcuts.capturing);
    assert!(!shell.user_shortcuts.unsupported);
    let draft_key = |shell: &StudioShell| {
        shell
            .user_shortcuts
            .draft
            .as_ref()
            .unwrap()
            .key
            .clone()
            .unwrap()
    };
    assert_eq!(
        draft_key(&shell),
        windowed(
            chord_key(
                key("K", true, false, false),
                key("MouseForward", false, false, false)
            ),
            "mixer"
        )
    );
    assert_eq!(key_text(&draft_key(&shell)), "Ctrl+K, MouseForward");
    // As the first stroke of a single key it keeps the window.
    shell.user_shortcuts.draft.as_mut().unwrap().key =
        Some(windowed(key("K", true, false, false), "mixer"));
    shell.user_shortcuts.capturing = true;
    shell.user_shortcuts.capturing_then = false;
    frame(
        &mut shell,
        mouse_event(egui::PointerButton::Extra2, press(false, true, false)),
    );
    assert!(!shell.user_shortcuts.unsupported);
    assert_eq!(
        draft_key(&shell),
        windowed(key("MouseForward", false, true, false), "mixer")
    );
    assert_eq!(
        save_block(
            &state,
            &audit_shortcuts(),
            1,
            shell.user_shortcuts.draft.as_ref().unwrap(),
            true
        ),
        None
    );
    assert_eq!(
        key_words(Locale::ENGLISH, &draft_key(&shell), false),
        "Shift+Mouse forward in the Audio mixer window"
    );
    let spanish = key_words(
        Locale::new(fm_ui_model::i18n::Language::Spanish),
        &draft_key(&shell),
        true,
    );
    assert!(
        spanish.starts_with("Mayús+Ratón adelante en la ventana de "),
        "{spanish}"
    );
}

/// The editor's scopes are every panel and every panel's pop-out window,
/// each a scope the protocol accepts, and a mouse button clashes with a
/// key or button in an overlapping scope like any key.
#[test]
fn window_scopes_name_every_panel_window_and_clash_where_they_overlap() {
    let scopes: Vec<_> = AnswerScope::all().collect();
    assert_eq!(
        scopes.len(),
        2 * crate::dock::PanelId::ALL.len() + crate::dock::ToolWindow::ALL.len()
    );
    for scope in scopes {
        let text = scope.key();
        assert!(fm_protocol::shortcut_scope_is_valid(&text), "{text}");
        assert_eq!(AnswerScope::of(&text), Some(scope));
    }
    assert_eq!(
        AnswerScope::of("mixer-window"),
        Some(AnswerScope::Window(crate::dock::PanelId::Mixer))
    );
    assert_eq!(AnswerScope::of("timeline-window"), None);

    let shortcuts = vec![bound(
        1,
        scoped(key("MouseBack", false, false, false), "mixer"),
    )];
    let in_window = windowed(key("MouseBack", false, false, false), "mixer");
    assert_eq!(
        key_taken(&shortcuts, 2, &in_window).map(|other| other.id),
        Some(1)
    );
    let elsewhere = windowed(key("MouseBack", false, false, false), "cues");
    assert_eq!(
        key_taken(&shortcuts, 2, &elsewhere).map(|other| other.id),
        None
    );
}

/// This console may give a shortcut a mouse button of its own, kept in
/// its preferences like a key.
#[test]
fn a_console_key_may_be_a_mouse_button() {
    let state = shortcuts_fixture(true);
    let overrides = ShortcutKeyOverrides::default();
    let keys = ConsoleKeys::of(&state, &overrides);
    let back = key("MouseBack", true, false, false);
    assert_eq!(
        local_capture(keys, 1, Captured::Key(back.clone())),
        Some(Ok(back.clone()))
    );
    let mut overrides = ShortcutKeyOverrides::default();
    assert!(overrides.set(project(), 1, Some(back.clone())));
    let lines = overrides.preference_entries();
    assert_eq!(lines[0].1, "Ctrl+MouseBack");
    let mut restored = ShortcutKeyOverrides::default();
    assert!(restored.apply_preference(&lines[0].0, &lines[0].1));
    assert_eq!(restored, overrides);
}

/// RC-004 dynamic values: a shortcut taking its gain from the firing
/// control fires only from an OSC message carrying a value; its key and
/// the editor's Fire now announce why they cannot.
#[test]
fn a_fader_shortcut_fires_only_with_a_control_value() {
    let fader = ShortcutStatus {
        spec: ShortcutSpec {
            osc: Some("fader".into()),
            action: WireShortcutAction::Actions(vec![WireCueAction::AudioGain {
                input: fm_types::InputTarget::Selected,
                gain: fm_protocol::CueGain::Fired {
                    min: -60_000,
                    max: 0,
                },
            }]),
            ..bound(7, key("F11", false, false, false)).spec
        },
        id: 7,
    };
    assert_eq!(
        value_block(&fader, None),
        Some(Msg::UserShortcutReasonNeedsValue.en())
    );
    assert_eq!(value_block(&fader, fm_protocol::FireValue::new(0)), None);
    assert_eq!(
        value_block(&bound(8, key("F12", false, false, false)), None),
        None
    );
    let state = crate::hover_audit::shortcuts_fixture_with(vec![fader]);
    let mut shell = StudioShell::default();
    // The key sends no value: nothing fires, and the console says why.
    assert_eq!(
        run_frame(
            &mut shell,
            &state,
            vec![key_event(Key::F11, Modifiers::NONE, false)],
            false
        ),
        Vec::new()
    );
    let said = toasts(&shell);
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("control that fires it"), "{said:?}");
    assert_eq!(shell.user_shortcut_osc_intent(&state, "fader", None), None);
    let value = fm_protocol::FireValue::new(640);
    assert_eq!(
        shell.user_shortcut_osc_intent(&state, "fader", value),
        Some(StudioIntent::Shortcut(ShortcutIntent::Fire {
            shortcut: 7,
            targets: fm_protocol::WireTargetBindings::default(),
            value,
        }))
    );
    // Every reason is translated.
    let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
    for msg in [
        Msg::UserShortcutReasonNeedsValue,
        Msg::CueParamGainModeHover,
        Msg::CueParamGainStepHover,
        Msg::CueParamGainMinHover,
        Msg::CueParamGainMaxHover,
        Msg::CueGainFired,
        Msg::CueReasonFiredOnlyInShortcuts,
        Msg::CueMuteToggle,
    ] {
        assert_ne!(spanish.text(msg), msg.en(), "{msg:?}");
    }
}

/// RC-004: this console's key may be a chord or scoped to a panel or a
/// pop-out window, as a project key may; it fires through the same press
/// handling, and clashes only where strokes and scopes overlap.
#[test]
fn console_keys_may_be_chords_and_scoped() {
    let shortcuts = vec![
        bound(1, key("F9", false, false, false)),
        bound(2, key("F10", false, false, false)),
        bound(3, key("F11", false, false, false)),
    ];
    let mut overrides = ShortcutKeyOverrides::default();
    let ctrl_k_1 = chord_key(key("K", true, false, false), key("1", false, false, false));
    assert!(overrides.set(project(), 1, Some(ctrl_k_1.clone())));
    assert!(overrides.set(
        project(),
        2,
        Some(scoped(key("F9", false, false, false), "mixer"))
    ));
    assert!(overrides.set(
        project(),
        3,
        Some(windowed(key("F9", false, false, false), "cues"))
    ));
    let keys = ConsoleKeys {
        shortcuts: &shortcuts,
        overrides: &overrides,
        project: Some(project()),
    };
    let ctrl_k = key_event(Key::K, press(true, false, false), false);
    let one = key_event(Key::Num1, Modifiers::NONE, false);
    let f9 = key_event(Key::F9, Modifiers::NONE, false);
    let mut chord = None;
    let mut events = vec![ctrl_k, one];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 0.0),
        vec![1]
    );
    // F9 is shortcut 2's in the Mixer panel and 3's in the Cues window;
    // shortcut 1 answers its console chord, not its project F9.
    let mut events = vec![f9.clone()];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 1.0),
        Vec::<u64>::new()
    );
    let mixer = KeyFocus {
        panel: Some("mixer"),
        window: None,
    };
    let mut events = vec![f9.clone()];
    assert_eq!(
        take_presses(&mut events, keys, mixer, &mut chord, 2.0),
        vec![2]
    );
    let cues_window = KeyFocus {
        panel: Some("cues"),
        window: Some("cues"),
    };
    let mut events = vec![f9];
    assert_eq!(
        take_presses(&mut events, keys, cues_window, &mut chord, 3.0),
        vec![3]
    );

    // A capture clashes only where strokes and scopes overlap.
    assert_eq!(
        local_capture(keys, 2, Captured::Key(ctrl_k_1.clone())),
        Some(Err(LocalRefusal::Taken("S1".into())))
    );
    let ctrl_k_2 = chord_key(key("K", true, false, false), key("2", false, false, false));
    assert_eq!(
        local_capture(keys, 2, Captured::Key(ctrl_k_2.clone())),
        Some(Ok(ctrl_k_2))
    );
    assert_eq!(
        local_capture(keys, 1, Captured::Key(key("F9", false, false, false))),
        Some(Err(LocalRefusal::Taken("S2".into())))
    );
    let in_titles = scoped(key("F9", false, false, false), "titles");
    assert_eq!(
        local_capture(keys, 1, Captured::Key(in_titles.clone())),
        Some(Ok(in_titles))
    );
}

/// A console key capture makes a chord's second stroke, or a new first
/// stroke keeping the key's chord and scope; a mouse button may be either
/// stroke.
#[test]
fn a_console_key_capture_keeps_the_chord_and_scope() {
    let mut chord = chord_key(key("K", true, false, false), key("1", false, false, false));
    chord.scope = Some("cues".into());
    let local = Some(chord.clone());
    let j = key("J", false, false, true);
    let back = key("MouseBack", false, false, false);
    assert_eq!(
        local_stroke(Some(&local), Captured::Key(j.clone()), false),
        Captured::Key(WireShortcutKey {
            then: chord.then.clone(),
            scope: Some("cues".into()),
            ..j.clone()
        })
    );
    assert_eq!(
        local_stroke(Some(&local), Captured::Key(back.clone()), false),
        Captured::Key(WireShortcutKey {
            then: chord.then.clone(),
            scope: Some("cues".into()),
            ..back.clone()
        })
    );
    assert_eq!(
        local_stroke(Some(&local), Captured::Key(j.clone()), true),
        Captured::Key(WireShortcutKey {
            then: Some(Box::new(j.clone())),
            ..chord.clone()
        })
    );
    assert_eq!(
        local_stroke(Some(&local), Captured::Key(back.clone()), true),
        Captured::Key(WireShortcutKey {
            then: Some(Box::new(back.clone())),
            ..chord
        })
    );
    // No console key yet: a second stroke has nothing to follow.
    assert_eq!(
        local_stroke(None, Captured::Key(j.clone()), true),
        Captured::Unsupported
    );
    assert_eq!(
        local_stroke(Some(&None), Captured::Key(j.clone()), false),
        Captured::Key(j)
    );
    assert_eq!(
        local_stroke(Some(&local), Captured::Cancel, true),
        Captured::Cancel
    );
}

/// "Add second key here" captures the console chord's second stroke into
/// this console's preferences; the reference and the list show it.
#[test]
fn add_second_key_here_makes_the_console_key_a_chord() {
    let state = shortcuts_fixture(false);
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    shell.open_user_shortcuts(&state);
    let mut console = scoped(key("K", true, false, false), "mixer-window");
    assert!(shell.shortcut_keys.set(project(), 1, Some(console.clone())));
    shell.user_shortcuts.capturing_local = true;
    shell.user_shortcuts.capturing_local_then = true;
    context.begin_pass(egui::RawInput {
        events: vec![key_event(Key::Num4, Modifiers::NONE, false)],
        ..Default::default()
    });
    let mut intents = Vec::new();
    shell.draw_user_shortcuts_window(&context, &state, &mut intents);
    let _ = context.end_pass();
    assert_eq!(intents, Vec::new());
    assert!(!shell.user_shortcuts.capturing_local);
    assert!(!shell.user_shortcuts.capturing_local_then);
    assert_eq!(shell.user_shortcuts.local_refusal, None);
    console.then = Some(Box::new(key("4", false, false, false)));
    assert_eq!(
        shell.shortcut_key_overrides().get(project(), 1),
        Some(&Some(console))
    );
    let keys = ConsoleKeys::of(&state, shell.shortcut_key_overrides());
    let row = row_text(Locale::ENGLISH, keys, &audit_shortcuts()[0]);
    assert!(row.starts_with("Lower third  Ctrl+K, 4"), "{row}");
    assert!(row.contains("(this console)"), "{row}");
    // The preference line keeps the chord and the window.
    assert!(
        shell
            .shortcut_key_overrides()
            .preference_entries()
            .contains(&(
                "shortcut.key.1.1".to_owned(),
                "Ctrl+K, 4 @mixer-window".to_owned()
            ))
    );
}

/// The tool windows are scopes the protocol accepts, by the names Studio
/// keeps them under, and the picker names each one in English and Spanish.
#[test]
fn tool_window_scopes_name_every_tool_window() {
    assert_eq!(
        crate::dock::ToolWindow::ALL.map(crate::dock::ToolWindow::key),
        fm_protocol::SHORTCUT_TOOL_WINDOWS
    );
    for tool in crate::dock::ToolWindow::ALL {
        let scope = AnswerScope::Tool(tool);
        let text = scope.key();
        assert!(fm_protocol::shortcut_scope_is_valid(&text), "{text}");
        assert_eq!(AnswerScope::of(&text), Some(scope));
        assert!(AnswerScope::all().any(|offered| offered == scope));
        assert_eq!(
            AnswerScope::hover(Some(scope), Msg::UserShortcutScopeHover),
            Msg::UserShortcutScopeToolHover
        );
        // A tool's name alone is no scope.
        assert_eq!(AnswerScope::of(tool.key()), None);
    }
    assert_eq!(
        AnswerScope::Tool(crate::dock::ToolWindow::Color).label(Locale::ENGLISH),
        "Colour grade window"
    );
    assert_eq!(
        AnswerScope::Tool(crate::dock::ToolWindow::Designer)
            .label(Locale::new(fm_ui_model::i18n::Language::Spanish)),
        "Ventana de Diseñador de capas"
    );
    let words = key_words(
        Locale::ENGLISH,
        &windowed(key("F9", false, false, false), "animation"),
        false,
    );
    assert_eq!(words, "F9 in the Animation window");
}

/// A key scoped to a tool window answers only while that window has the
/// focus: not in a panel, a panel's window, or another tool's window.
#[test]
fn a_tool_window_scoped_key_answers_only_in_its_tool_window() {
    let shortcuts = vec![
        bound(1, windowed(key("F11", false, false, false), "color")),
        bound(2, windowed(key("F11", false, false, false), "animation")),
        bound(3, scoped(key("F11", false, false, false), "mixer")),
        bound(
            4,
            windowed(key("MouseBack", false, false, false), "designer"),
        ),
        bound(5, key("F9", false, false, false)),
    ];
    let overrides = ShortcutKeyOverrides::default();
    let keys = ConsoleKeys {
        shortcuts: &shortcuts,
        overrides: &overrides,
        project: None,
    };
    let presses = || {
        vec![
            key_event(Key::F11, Modifiers::NONE, false),
            mouse_event(egui::PointerButton::Extra1, Modifiers::NONE),
            key_event(Key::F9, Modifiers::NONE, false),
        ]
    };
    let mut chord = None;
    for (focus, fired) in [
        (
            KeyFocus {
                panel: None,
                window: Some("color"),
            },
            vec![1, 5],
        ),
        (
            KeyFocus {
                panel: None,
                window: Some("animation"),
            },
            vec![2, 5],
        ),
        (
            KeyFocus {
                panel: None,
                window: Some("designer"),
            },
            vec![4, 5],
        ),
        (
            KeyFocus {
                panel: Some("mixer"),
                window: Some("mixer"),
            },
            vec![3, 5],
        ),
        (KeyFocus::default(), vec![5]),
    ] {
        let mut events = presses();
        assert_eq!(
            take_presses(&mut events, keys, focus, &mut chord, 0.0),
            fired,
            "{focus:?}"
        );
    }
}

/// Runs one main-window frame of the project's shortcut keys, then tool
/// `tool`'s own read of them (`native` in its own window).
fn run_tool_frame(
    shell: &mut StudioShell,
    state: &StudioUiState,
    events: Vec<Event>,
    tool: crate::dock::ToolWindow,
    native: bool,
) -> (Vec<StudioIntent>, Vec<StudioIntent>) {
    let context = egui::Context::default();
    context.begin_pass(egui::RawInput {
        events,
        ..Default::default()
    });
    let mut ui = egui::Ui::new(
        context.clone(),
        egui::Id::new("studio-tool-keys"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(context.viewport_rect()),
    );
    let mut main = Vec::new();
    if !native {
        shell.fire_user_shortcut_keys(&mut ui, state, None, &mut main);
    }
    let mut in_tool = Vec::new();
    shell.tool_window_keys(&mut ui, state, tool, (native, None), &mut in_tool);
    drop(ui);
    let _ = context.end_pass();
    (main, in_tool)
}

/// Studio's dispatcher gives the main window's presses to the tool window
/// or embedded pop-out the operator last clicked, while it is still there:
/// its window-scoped keys answer and the focused panel's do not.
#[test]
fn the_main_window_answers_the_tool_window_or_embedded_pop_out_with_the_focus() {
    use crate::dock::{PanelId, ToolWindow};
    let state = crate::hover_audit::shortcuts_fixture_with(vec![
        bound(4, windowed(key("F11", false, false, false), "color")),
        bound(5, windowed(key("F11", false, false, false), "animation")),
        bound(6, scoped(key("F11", false, false, false), "mixer")),
        bound(7, windowed(key("F12", false, false, false), "mixer")),
    ]);
    let presses = || {
        vec![
            key_event(Key::F11, Modifiers::NONE, false),
            key_event(Key::F12, Modifiers::NONE, false),
        ]
    };
    let mut shell = StudioShell {
        focused_panel: Some(PanelId::Mixer),
        ..StudioShell::default()
    };
    assert_eq!(
        run_frame(&mut shell, &state, presses(), false),
        vec![fire(6)]
    );

    // The Colour window over the console has the focus.
    shell.color.open = true;
    shell.focused_window = Some(KeyWindow::Tool(ToolWindow::Color));
    assert_eq!(
        run_frame(&mut shell, &state, presses(), false),
        vec![fire(4)]
    );
    // The Animation editor, popped out but embedded in the main window.
    shell.animation.open = true;
    shell.layout.set_tool_popped(ToolWindow::Animation, true);
    shell.focused_window = Some(KeyWindow::Tool(ToolWindow::Animation));
    assert_eq!(
        run_frame(&mut shell, &state, presses(), false),
        vec![fire(5)]
    );
    // A closed tool window has no focus: the panel's keys answer again.
    shell.color.open = false;
    shell.focused_window = Some(KeyWindow::Tool(ToolWindow::Color));
    assert_eq!(
        run_frame(&mut shell, &state, presses(), false),
        vec![fire(6)]
    );

    // The Mixer popped out but embedded: its panel's and window's keys.
    assert!(shell.layout.dock.pop_out(PanelId::Mixer, None));
    shell.focused_window = Some(KeyWindow::Panel(PanelId::Mixer));
    assert_eq!(
        run_frame(&mut shell, &state, presses(), false),
        vec![fire(6), fire(7)]
    );
    // Docked back, it is a panel again.
    shell.layout.dock.dock_back(PanelId::Mixer);
    assert_eq!(
        run_frame(&mut shell, &state, presses(), false),
        vec![fire(6)]
    );
}

/// A tool window that owns the keyboard reads the project's keys itself,
/// after its own keys took theirs; in its own native window it always
/// does. Either way it answers its own window's keys and unscoped ones.
#[test]
fn a_tool_window_reads_its_keys_when_it_owns_the_keyboard_or_its_window() {
    use crate::dock::ToolWindow;
    let state = crate::hover_audit::shortcuts_fixture_with(vec![
        bound(4, windowed(key("F11", false, false, false), "color")),
        bound(5, windowed(key("F11", false, false, false), "designer")),
        bound(6, scoped(key("F11", false, false, false), "mixer")),
    ]);
    let presses = || {
        vec![
            key_event(Key::F11, Modifiers::NONE, false),
            key_event(Key::F9, Modifiers::NONE, false),
        ]
    };
    let mut shell = StudioShell::default();
    shell.color.open = true;
    shell.focused_window = Some(KeyWindow::Tool(ToolWindow::Color));
    // Not owning the keyboard, the main window read them.
    assert_eq!(
        run_tool_frame(&mut shell, &state, presses(), ToolWindow::Color, false),
        (vec![fire(4), fire(1)], Vec::new())
    );
    // Owning it, the window reads them after its own keys.
    shell.color.panel.keyboard_active = true;
    assert_eq!(
        run_tool_frame(&mut shell, &state, presses(), ToolWindow::Color, false),
        (Vec::new(), vec![fire(4), fire(1)])
    );
    // Another tool sharing the main window without the focus reads none.
    shell.designer.open = true;
    assert_eq!(
        run_tool_frame(&mut shell, &state, presses(), ToolWindow::Designer, false),
        (Vec::new(), Vec::new())
    );
    // In its own native window the designer reads its own keys.
    assert_eq!(
        run_tool_frame(&mut shell, &state, presses(), ToolWindow::Designer, true),
        (Vec::new(), vec![fire(5), fire(1)])
    );
}

/// A press inside a window sharing the main window's input gives it the
/// focus; a press elsewhere, or outside its docked area, does not.
#[test]
fn a_press_in_a_window_gives_it_the_focus() {
    use crate::dock::ToolWindow;
    let context = egui::Context::default();
    let mut shell = StudioShell::default();
    let area_id = egui::Id::new("studio-test-tool-window");
    let frame = |shell: &mut StudioShell, events: Vec<Event>, docked: Option<egui::Rect>| {
        context.begin_pass(egui::RawInput {
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        });
        egui::Area::new(area_id)
            .fixed_pos(egui::pos2(100.0, 100.0))
            .show(&context, |ui| {
                ui.allocate_space(egui::vec2(200.0, 200.0));
                shell.focus_window_on_press(ui, docked, KeyWindow::Tool(ToolWindow::Designer));
            });
        let _ = context.end_pass();
    };
    let press_at = |x: f32, y: f32| {
        let pos = egui::pos2(x, y);
        vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ]
    };
    // The window is laid out once before it can be hit.
    frame(&mut shell, Vec::new(), None);
    frame(&mut shell, press_at(20.0, 20.0), None);
    assert_eq!(shell.focused_window, None);
    frame(&mut shell, press_at(150.0, 150.0), None);
    assert_eq!(
        shell.focused_window,
        Some(KeyWindow::Tool(ToolWindow::Designer))
    );
    // Outside the docked area the press is the console's.
    shell.focused_window = None;
    let docked = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(20.0, 20.0));
    frame(&mut shell, press_at(150.0, 150.0), Some(docked));
    assert_eq!(shell.focused_window, None);
}
