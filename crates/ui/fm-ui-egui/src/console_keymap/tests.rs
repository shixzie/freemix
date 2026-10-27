use egui::{Key, Modifiers};

use super::{
    BuiltinProfile, ConsoleAction, ConsoleKeyProfiles, ConsoleKeymap, ConsoleStroke, CustomProfile,
    KeyRefusal, MAX_CUSTOM_PROFILES, NUMBERED_INPUTS, ProfileChoice,
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
    assert_eq!(all.len(), 3 * usize::from(NUMBERED_INPUTS) + 3 + 4 + 1);
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
    assert_eq!(keys, ["1 – 8", "C", "F", "B", "F5 – F8", "G"]);
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
            "G"
        ]
    );
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
