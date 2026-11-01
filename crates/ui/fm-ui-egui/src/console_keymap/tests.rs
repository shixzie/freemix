use egui::{Key, Modifiers};

use super::{
    BuiltinProfile, ConsoleAction, ConsoleKeyProfiles, ConsoleKeymap, ConsoleStroke, CustomProfile,
    KeyFileError, KeyRefusal, MAX_CUSTOM_PROFILES, MAX_KEY_FILE_BYTES, NUMBERED_INPUTS,
    ProfileChoice,
};
use crate::l10n::Locale;

/// A press with Ctrl/Cmd, Shift, and Alt as given, as egui reports it on
/// Linux and Windows.
const fn press(command: bool, shift: bool, alt: bool) -> Modifiers {
    Modifiers {
        alt,
        ctrl: command,
        shift,
        mac_cmd: false,
        command,
    }
}

/// Every action's token parses back, and every profile names each key at
/// most once and each action at most once.
#[test]
fn tokens_round_trip_and_profiles_are_unambiguous() {
    let all = ConsoleAction::all();
    assert_eq!(
        all.len(),
        3 * usize::from(NUMBERED_INPUTS) + 3 + 4 + 4 + 1 + 8 + 5 + 2
    );
    for action in &all {
        assert_eq!(ConsoleAction::from_token(&action.token()), Some(*action));
    }
    assert_eq!(ConsoleAction::from_token("preview.10"), None);
    for profile in BuiltinProfile::ALL {
        let keymap = profile.keymap();
        for (index, (action, stroke)) in keymap.bindings().iter().enumerate() {
            assert!(
                keymap.bindings()[..index]
                    .iter()
                    .all(|(other, key)| other != action && key != stroke),
                "{profile:?}: {action:?} on {}",
                stroke.text()
            );
            // No built-in key is one the console keeps outside profiles,
            // except Space and Enter, which profiles may take.
            assert_eq!(
                crate::user_shortcuts::fixed_console_key(*stroke),
                None,
                "{profile:?}: {}",
                stroke.text()
            );
        }
        assert_eq!(BuiltinProfile::from_token(profile.token()), Some(profile));
    }
}

/// `FreeMix` keeps the console's keys exactly; vMix style follows vMix's
/// shortcut functions.
#[test]
fn builtin_profiles_answer_their_keys() {
    let freemix = ConsoleKeymap::default();
    assert_eq!(freemix, BuiltinProfile::FreeMix.keymap());
    for (key, action) in [
        (Key::Num1, ConsoleAction::Preview(1)),
        (Key::Num8, ConsoleAction::Preview(8)),
        (Key::C, ConsoleAction::Cut),
        (Key::F, ConsoleAction::Fade),
        (Key::B, ConsoleAction::FadeToBlack),
        (Key::F5, ConsoleAction::Favorite(1)),
        (Key::F8, ConsoleAction::Favorite(4)),
        (Key::G, ConsoleAction::CueGo),
    ] {
        assert_eq!(
            freemix.action(key, Modifiers::NONE),
            Some(action),
            "{key:?}"
        );
        // Exact modifiers: Shift or Ctrl make it another key.
        assert_eq!(freemix.action(key, press(false, true, false)), None);
        assert_eq!(freemix.action(key, press(true, false, false)), None);
    }
    assert_eq!(freemix.action(Key::Num9, Modifiers::NONE), None);
    assert_eq!(freemix.stroke(ConsoleAction::Take(1)), None);

    let vmix = BuiltinProfile::Vmix.keymap();
    for (key, pressed, action) in [
        (
            Key::Num9,
            press(false, false, false),
            ConsoleAction::Preview(9),
        ),
        (Key::Num3, press(true, false, false), ConsoleAction::Take(3)),
        (
            Key::Num3,
            press(false, false, true),
            ConsoleAction::QuickPlay(3),
        ),
        (Key::Space, press(false, false, false), ConsoleAction::Cut),
        (Key::Enter, press(false, false, false), ConsoleAction::Fade),
        (
            Key::B,
            press(true, false, false),
            ConsoleAction::FadeToBlack,
        ),
        (
            Key::F2,
            press(true, false, false),
            ConsoleAction::Favorite(2),
        ),
        (Key::G, press(false, false, false), ConsoleAction::CueGo),
    ] {
        assert_eq!(vmix.action(key, pressed), Some(action), "{key:?}");
    }
    // macOS reports Cmd as `mac_cmd` with `command`: still the command key.
    assert_eq!(
        vmix.action(Key::Num3, Modifiers::MAC_CMD | Modifiers::COMMAND),
        Some(ConsoleAction::Take(3))
    );
    // macOS's Control alone is not the command key.
    assert_eq!(vmix.action(Key::Num3, Modifiers::CTRL), None);
    assert_eq!(vmix.action(Key::C, Modifiers::NONE), None);
}

#[test]
fn strokes_write_and_parse_like_shortcut_keys() {
    for (stroke, text) in [
        (ConsoleStroke::plain(Key::C), "C"),
        (ConsoleStroke::command(Key::Num1), "Ctrl+1"),
        (ConsoleStroke::alt(Key::F4), "Alt+F4"),
        (
            ConsoleStroke {
                key: Key::F9,
                command: true,
                shift: true,
                alt: true,
            },
            "Ctrl+Shift+Alt+F9",
        ),
        (ConsoleStroke::plain(Key::Space), "Space"),
    ] {
        assert_eq!(stroke.text(), text);
        assert_eq!(ConsoleStroke::parse(text), Some(stroke), "{text}");
    }
    for bad in [
        "",
        "Ctrl+",
        "Shift+Ctrl+C",
        "Ctrl+Ctrl+C",
        "Hyper+C",
        "c",
        "Nope",
    ] {
        assert_eq!(ConsoleStroke::parse(bad), None, "{bad}");
    }
}

/// The reference folds numbered runs into one row.
#[test]
fn reference_rows_fold_numbered_runs() {
    let l = Locale::ENGLISH;
    let freemix = ConsoleKeymap::default().reference_rows(l);
    let keys: Vec<&str> = freemix.iter().map(|(keys, _)| keys.as_str()).collect();
    assert_eq!(
        keys,
        [
            "1 – 8",
            "C",
            "F",
            "B",
            "F5 – F8",
            "G",
            "Shift+Right",
            "Shift+Left",
            "I",
            "O",
            "E",
            "K",
            "Left",
            "Right",
            "J",
            "L"
        ]
    );
    let vmix = BuiltinProfile::Vmix.keymap().reference_rows(l);
    let keys: Vec<&str> = vmix.iter().map(|(keys, _)| keys.as_str()).collect();
    assert_eq!(
        keys,
        [
            "1 – 9",
            "Ctrl+1 – 9",
            "Alt+1 – 9",
            "Space",
            "Enter",
            "Ctrl+B",
            "Ctrl+F1 – F4",
            "F9 – F12",
            "G",
            "Shift+Right",
            "Shift+Left",
            "I",
            "O",
            "E",
            "K",
            "Left",
            "Right",
            "J",
            "L"
        ]
    );
    assert_eq!(
        vmix[7].1,
        "Put the Preview input on the numbered overlay, or take it off"
    );
    assert_eq!(vmix[13].1, "Capture the marked replay event");
    // A broken run lists its keys one by one.
    let mut profiles = ConsoleKeyProfiles::default();
    let ProfileChoice::Custom(index) = profiles
        .add("Odd", ProfileChoice::Builtin(BuiltinProfile::FreeMix))
        .unwrap()
    else {
        panic!("a custom profile");
    };
    profiles
        .set_key(
            index,
            ConsoleAction::Preview(2),
            Some(ConsoleStroke::plain(Key::Q)),
        )
        .unwrap();
    let rows = profiles
        .keymap(ProfileChoice::Custom(index))
        .unwrap()
        .reference_rows(l);
    assert_eq!(rows[0], ("1".to_owned(), "Input 1 to Preview".to_owned()));
    assert_eq!(rows[1], ("Q".to_owned(), "Input 2 to Preview".to_owned()));
    assert_eq!(rows[2].0, "3 – 8");
}

#[test]
fn custom_profiles_change_keys_within_the_rules() {
    let mut profiles = ConsoleKeyProfiles::default();
    assert_eq!(
        profiles.active(),
        ProfileChoice::Builtin(BuiltinProfile::FreeMix)
    );
    let choice = profiles
        .add("Late show", ProfileChoice::Builtin(BuiltinProfile::Vmix))
        .unwrap();
    let ProfileChoice::Custom(index) = choice else {
        panic!("a custom profile");
    };
    assert!(profiles.choose(choice));
    assert_eq!(profiles.active_keymap(), BuiltinProfile::Vmix.keymap());

    // A free key moves an action; the old key frees up.
    profiles
        .set_key(
            index,
            ConsoleAction::Cut,
            Some(ConsoleStroke::plain(Key::X)),
        )
        .unwrap();
    let keymap = profiles.active_keymap();
    assert_eq!(
        keymap.action(Key::X, Modifiers::NONE),
        Some(ConsoleAction::Cut)
    );
    assert_eq!(keymap.action(Key::Space, Modifiers::NONE), None);
    assert!(profiles.custom()[index].changed(ConsoleAction::Cut));

    // Another action's key, a console key, and built-in profiles refuse.
    assert_eq!(
        profiles.set_key(
            index,
            ConsoleAction::Fade,
            Some(ConsoleStroke::plain(Key::X))
        ),
        Err(KeyRefusal::Taken(ConsoleAction::Cut))
    );
    assert_eq!(
        profiles.set_key(
            index,
            ConsoleAction::Fade,
            Some(ConsoleStroke::plain(Key::T))
        ),
        Err(KeyRefusal::Fixed("T"))
    );
    assert!(matches!(
        profiles.set_key(
            index,
            ConsoleAction::Fade,
            Some(ConsoleStroke::plain(Key::F1))
        ),
        Err(KeyRefusal::Fixed(_))
    ));
    assert_eq!(
        profiles.set_key(9, ConsoleAction::Fade, None),
        Err(KeyRefusal::Missing)
    );

    // No key, then back to the base key.
    profiles.set_key(index, ConsoleAction::CueGo, None).unwrap();
    assert_eq!(profiles.active_keymap().stroke(ConsoleAction::CueGo), None);
    profiles.reset_key(index, ConsoleAction::CueGo).unwrap();
    assert!(!profiles.custom()[index].changed(ConsoleAction::CueGo));
    profiles.reset_key(index, ConsoleAction::Cut).unwrap();
    assert_eq!(profiles.active_keymap(), BuiltinProfile::Vmix.keymap());

    // Names and the limit.
    assert!(
        profiles
            .add(" ", ProfileChoice::Builtin(BuiltinProfile::FreeMix))
            .is_none()
    );
    assert!(profiles.rename(index, "Early show"));
    assert!(!profiles.rename(index, "a\nb"));
    while profiles.custom().len() < MAX_CUSTOM_PROFILES {
        assert!(profiles.add("More", choice).is_some());
    }
    assert!(profiles.add("Too many", choice).is_none());

    // Removing the profile in use falls back to FreeMix.
    assert!(profiles.remove(index));
    assert_eq!(
        profiles.active(),
        ProfileChoice::Builtin(BuiltinProfile::FreeMix)
    );
    assert_eq!(profiles.custom().len(), MAX_CUSTOM_PROFILES - 1);
}

/// A key moved from one action to another, or two swapped keys, read back
/// whichever action the file lists first.
#[test]
fn moved_and_swapped_keys_read_back() {
    let mut desk = ConsoleKeyProfiles::default();
    desk.add("Moved", ProfileChoice::Builtin(BuiltinProfile::Vmix))
        .unwrap();
    // Cut takes Fade's Enter, and Fade takes F8.
    desk.set_key(0, ConsoleAction::Fade, Some(ConsoleStroke::plain(Key::F8)))
        .unwrap();
    desk.set_key(
        0,
        ConsoleAction::Cut,
        Some(ConsoleStroke::plain(Key::Enter)),
    )
    .unwrap();
    // Overlay 1 and overlay 2 swap F9 and F10 through a free key.
    let swap = |desk: &mut ConsoleKeyProfiles, action, key| {
        desk.set_key(0, action, Some(ConsoleStroke::plain(key)))
            .unwrap();
    };
    swap(&mut desk, ConsoleAction::Overlay(1), Key::F7);
    swap(&mut desk, ConsoleAction::Overlay(2), Key::F9);
    swap(&mut desk, ConsoleAction::Overlay(1), Key::F10);
    let mut read = ConsoleKeyProfiles::default();
    for (key, value) in desk.preference_entries() {
        assert!(read.apply_preference(&key, &value), "{key}");
    }
    read.finish_preferences();
    assert_eq!(read, desk);
    let text = desk
        .export_text(Locale::ENGLISH, ProfileChoice::Custom(0))
        .unwrap();
    let mut imported = ConsoleKeyProfiles::default();
    imported.import_text(&text).unwrap();
    assert_eq!(imported.custom()[0], desk.custom()[0]);
    let keymap = imported.keymap(ProfileChoice::Custom(0)).unwrap();
    assert_eq!(
        keymap.stroke(ConsoleAction::Cut),
        Some(ConsoleStroke::plain(Key::Enter))
    );
    assert_eq!(
        keymap.stroke(ConsoleAction::Overlay(1)),
        Some(ConsoleStroke::plain(Key::F10))
    );
    assert_eq!(
        keymap.stroke(ConsoleAction::Overlay(2)),
        Some(ConsoleStroke::plain(Key::F9))
    );
}

#[test]
fn profiles_travel_through_key_files() {
    let l = Locale::ENGLISH;
    let mut desk = ConsoleKeyProfiles::default();
    let late = desk
        .add("Late show", ProfileChoice::Builtin(BuiltinProfile::Vmix))
        .unwrap();
    desk.set_key(0, ConsoleAction::Cut, Some(ConsoleStroke::command(Key::K)))
        .unwrap();
    desk.set_key(0, ConsoleAction::CueGo, None).unwrap();
    let text = desk.export_text(l, late).unwrap();
    assert_eq!(
        text,
        "# FreeMix key profile\nname = Late show\nbase = vmix\ncut = Ctrl+K\ncue_go = none\n"
    );

    // Another console reads it as one more profile of its own, without
    // choosing it.
    let mut other = ConsoleKeyProfiles::default();
    other
        .add("Mine", ProfileChoice::Builtin(BuiltinProfile::FreeMix))
        .unwrap();
    assert_eq!(other.import_text(&text), Ok(ProfileChoice::Custom(1)));
    assert_eq!(other.custom()[1], desk.custom()[0]);
    assert_eq!(other.active(), ProfileChoice::default());

    // A built-in profile exports as an unchanged copy of itself.
    let vmix = desk
        .export_text(l, ProfileChoice::Builtin(BuiltinProfile::Vmix))
        .unwrap();
    assert_eq!(
        vmix,
        "# FreeMix key profile\nname = vMix style\nbase = vmix\n"
    );
    assert_eq!(desk.export_text(l, ProfileChoice::Custom(5)), None);

    // Blank lines, comments, and unusable lines are skipped.
    let mut read = ConsoleKeyProfiles::default();
    let loose =
        "\n# hand edited\n  name =  Booth \nbase = vmix\nwarp = W\ncut = Shift+Ctrl+C\nfade = F8\n";
    assert_eq!(read.import_text(loose), Ok(ProfileChoice::Custom(0)));
    let booth = &read.custom()[0];
    assert_eq!(booth.name, "Booth");
    assert_eq!(booth.base, BuiltinProfile::Vmix);
    assert_eq!(
        booth.keymap().stroke(ConsoleAction::Fade),
        Some(ConsoleStroke::plain(Key::F8))
    );

    // A file naming no profile, one too long, or one past the limit adds
    // nothing.
    let mut read = ConsoleKeyProfiles::default();
    assert_eq!(
        read.import_text("base = vmix\n"),
        Err(KeyFileError::Invalid)
    );
    let long = format!("name = Big\n#{}\n", "x".repeat(MAX_KEY_FILE_BYTES));
    assert_eq!(read.import_text(&long), Err(KeyFileError::Invalid));
    assert!(read.custom().is_empty());
    for _ in 0..MAX_CUSTOM_PROFILES {
        read.import_text(&text).unwrap();
    }
    assert_eq!(read.import_text(&text), Err(KeyFileError::Full));
    assert_eq!(read.custom().len(), MAX_CUSTOM_PROFILES);
}

#[test]
fn profiles_round_trip_through_preference_lines() {
    let mut profiles = ConsoleKeyProfiles::default();
    assert!(profiles.preference_entries().is_empty());
    profiles
        .add(
            "FreeMix copy",
            ProfileChoice::Builtin(BuiltinProfile::FreeMix),
        )
        .unwrap();
    let late = profiles
        .add("Late show", ProfileChoice::Builtin(BuiltinProfile::Vmix))
        .unwrap();
    profiles
        .set_key(1, ConsoleAction::Cut, Some(ConsoleStroke::command(Key::K)))
        .unwrap();
    profiles.set_key(1, ConsoleAction::CueGo, None).unwrap();
    assert!(profiles.choose(late));
    let entries = profiles.preference_entries();
    let lines: Vec<String> = entries
        .iter()
        .map(|(key, value)| format!("{key} = {value}"))
        .collect();
    assert_eq!(
        lines,
        [
            "keymap.profile = custom.2",
            "keymap.custom.1.name = FreeMix copy",
            "keymap.custom.1.base = freemix",
            "keymap.custom.2.name = Late show",
            "keymap.custom.2.base = vmix",
            "keymap.custom.2.cut = Ctrl+K",
            "keymap.custom.2.cue_go = none",
        ]
    );
    let mut read = ConsoleKeyProfiles::default();
    for (key, value) in entries.iter().rev() {
        assert!(read.apply_preference(key, value), "{key}");
    }
    read.finish_preferences();
    assert_eq!(read, profiles);

    // Unusable lines are skipped; a profile without a name is dropped with
    // the choice of it, and a change onto a taken key is forgotten.
    let mut read = ConsoleKeyProfiles::default();
    for (key, value) in [
        ("keymap.profile", "custom.2"),
        ("keymap.custom.1.name", "Kept"),
        ("keymap.custom.1.base", "vmix"),
        ("keymap.custom.1.fade", "Space"),
        ("keymap.custom.1.cut", "T"),
        ("keymap.custom.2.base", "freemix"),
    ] {
        read.apply_preference(key, value);
    }
    for (key, value) in [
        ("keymap.profile", "lol"),
        ("keymap.custom.0.name", "Zero"),
        ("keymap.custom.9.name", "Nine"),
        ("keymap.custom.1.base", "obs"),
        ("keymap.custom.1.warp", "W"),
        ("keymap.custom.1.cut", "Shift+Ctrl+C"),
        ("keymap.other", "1"),
    ] {
        assert!(!read.apply_preference(key, value), "{key} = {value}");
    }
    read.finish_preferences();
    assert_eq!(read.custom().len(), 1);
    assert_eq!(
        read.custom()[0],
        CustomProfile::new("Kept", BuiltinProfile::Vmix)
    );
    assert_eq!(
        read.active(),
        ProfileChoice::Builtin(BuiltinProfile::FreeMix)
    );
}
