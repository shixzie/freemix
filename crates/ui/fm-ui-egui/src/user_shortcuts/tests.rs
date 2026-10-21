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

/// A mouse button fires its shortcut at once with exactly its modifiers,
/// takes only its own press, ends a waiting chord, and never ends a chord
/// itself; the primary button stays the console's.
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

    // A bound button ends a waiting chord and fires.
    let mut events = vec![
        key_event(Key::K, press(true, false, false), false),
        mouse_event(egui::PointerButton::Extra1, Modifiers::NONE),
    ];
    assert_eq!(
        take_presses(&mut events, keys, KeyFocus::default(), &mut chord, 1.0),
        vec![1]
    );
    assert_eq!(chord, None);
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

/// A captured mouse button is a single press: it replaces a chord's
/// strokes but keeps the scope, and is refused as a second stroke.
#[test]
fn set_key_binds_a_mouse_button_as_a_single_press() {
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
    // A button as the second stroke is refused, keeping the chord.
    shell.user_shortcuts.capturing = true;
    shell.user_shortcuts.capturing_then = true;
    frame(
        &mut shell,
        mouse_event(egui::PointerButton::Extra2, Modifiers::NONE),
    );
    assert!(!shell.user_shortcuts.capturing);
    assert!(shell.user_shortcuts.unsupported);
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
    assert_eq!(key_text(&draft_key(&shell)), "Ctrl+K, 1");
    // As the first stroke it drops the second and keeps the window.
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
    assert_eq!(scopes.len(), 2 * crate::dock::PanelId::ALL.len());
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
