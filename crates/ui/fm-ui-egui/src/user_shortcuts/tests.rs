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
    shell.fire_user_shortcut_keys(&mut ui, state, &mut intents);
    drop(ui);
    let _ = context.end_pass();
    intents
}

fn fire(shortcut: u64) -> StudioIntent {
    StudioIntent::Shortcut(ShortcutIntent::Fire { shortcut })
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
        shell.user_shortcut_osc_intent(&state, "lower-third"),
        Some(fire(1))
    );
    assert_eq!(
        shell.user_shortcut_osc_intent(&state, "open"),
        Some(fire(2))
    );
    assert_eq!(shell.user_shortcut_osc_intent(&state, "unknown"), None);
    assert_eq!(toasts(&shell), Vec::<String>::new());
    let viewer = shortcuts_fixture(false);
    assert_eq!(shell.user_shortcut_osc_intent(&viewer, "open"), None);
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
                        input: fm_protocol::WireInputId::from_domain(fm_types::InputId::new(
                            core::num::NonZeroU128::new(2).unwrap(),
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
    let rows = reference_rows(Locale::ENGLISH, &state);
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
    let spanish = reference_rows(Locale::new(fm_ui_model::i18n::Language::Spanish), &state);
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
