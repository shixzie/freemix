//! User shortcuts (RC-004): each definition's bounds, and the project's
//! shortcut set (ids, keys, and OSC names unique).

use core::num::NonZeroU128;

use fm_model::{
    CueAction, MAX_SHORTCUT_ACTIONS, MAX_SHORTCUT_TEXT_BYTES, MAX_SHORTCUTS, SHORTCUT_OSC_PREFIX,
    Shortcut, ShortcutAction, ShortcutCueOperation, ShortcutError, ShortcutKey, TitleFieldId,
    TransitionEffect, ValidationErrorKind, validate_shortcut_definition, validate_shortcuts,
};
use fm_types::InputId;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn shortcut(id: u64) -> Shortcut {
    Shortcut {
        id,
        name: format!("Shortcut {id}"),
        enabled: true,
        key: Some(ShortcutKey::plain(format!("F{id}"))),
        osc: Some(format!("s-{id}")),
        action: ShortcutAction::Actions(vec![CueAction::Preview {
            input: fm_types::InputTarget::Fixed(input(3)),
        }]),
    }
}

fn with_action(action: ShortcutAction) -> Shortcut {
    Shortcut {
        action,
        ..shortcut(1)
    }
}

#[test]
fn a_definition_is_bounded() {
    assert_eq!(validate_shortcut_definition(&shortcut(1)), Ok(()));
    for name in ["", "   ", "tab\there", &"x".repeat(65)] {
        let invalid = Shortcut {
            name: name.to_owned(),
            ..shortcut(1)
        };
        assert_eq!(
            validate_shortcut_definition(&invalid),
            Err(ShortcutError::InvalidName),
            "{name:?}"
        );
    }
    for key in ["", "F+9", "Ü", "ABCDEFGHIJKLMNOPQ", "Page Up"] {
        let invalid = Shortcut {
            key: Some(ShortcutKey::plain(key)),
            ..shortcut(1)
        };
        assert_eq!(
            validate_shortcut_definition(&invalid),
            Err(ShortcutError::InvalidKey),
            "{key:?}"
        );
    }
    // `-` alone is the wire's "no OSC name" (Protocol 2.72).
    for osc in ["", "-", "Intro", "in/tro", "in tro", &"a".repeat(33)] {
        let invalid = Shortcut {
            osc: Some(osc.to_owned()),
            ..shortcut(1)
        };
        assert_eq!(
            validate_shortcut_definition(&invalid),
            Err(ShortcutError::InvalidOsc),
            "{osc:?}"
        );
    }
    assert_eq!(
        shortcut(1).osc_address().as_deref(),
        Some(format!("{SHORTCUT_OSC_PREFIX}s-1").as_str())
    );
    assert_eq!(
        ShortcutKey {
            key: "F9".to_owned(),
            command: true,
            shift: true,
            alt: true,
            then: None,
            scope: None,
        }
        .to_string(),
        "Ctrl+Shift+Alt+F9"
    );
}

#[test]
fn actions_run_on_one_frame_without_waits() {
    let preview = CueAction::Preview { input: input(3) };
    let take = CueAction::Take {
        input: Some(input(4)),
        effect: TransitionEffect::Cut,
        duration_frames: 0,
    };
    assert_eq!(
        validate_shortcut_definition(&with_action(ShortcutAction::Actions(Vec::new()))),
        Err(ShortcutError::InvalidActionCount)
    );
    assert_eq!(
        validate_shortcut_definition(&with_action(ShortcutAction::Actions(vec![
            preview
                .clone()
                .into();
            MAX_SHORTCUT_ACTIONS
                + 1
        ]))),
        Err(ShortcutError::InvalidActionCount)
    );
    assert_eq!(
        validate_shortcut_definition(&with_action(ShortcutAction::Actions(vec![
            preview.clone().into();
            MAX_SHORTCUT_ACTIONS
        ]))),
        Ok(())
    );
    for wait in [
        CueAction::Wait { frames: 5 },
        CueAction::Delay { millis: 200 },
    ] {
        assert_eq!(
            validate_shortcut_definition(&with_action(ShortcutAction::Actions(vec![
                preview.clone().into(),
                wait
            ]))),
            Err(ShortcutError::InvalidAction { action: 1 })
        );
    }
    // Out of range like a cue action: a cut over frames.
    assert_eq!(
        validate_shortcut_definition(&with_action(ShortcutAction::Actions(vec![
            CueAction::Take {
                input: None,
                effect: TransitionEffect::Cut,
                duration_frames: 3,
            }
        ]))),
        Err(ShortcutError::InvalidAction { action: 0 })
    );
    assert_eq!(
        validate_shortcut_definition(&with_action(ShortcutAction::Actions(vec![
            take.clone().into(),
            CueAction::PlaylistStart
        ]))),
        Err(ShortcutError::ConflictingTakes)
    );
    let text = |bytes: usize| CueAction::TitleText {
        input: fm_types::InputTarget::Fixed(input(5)),
        field: TitleFieldId::new(NonZeroU128::new(1).unwrap()),
        text: "x".repeat(bytes),
    };
    let fits: Vec<CueAction> = (0..MAX_SHORTCUT_TEXT_BYTES / 128)
        .map(|_| text(128))
        .collect();
    assert_eq!(
        validate_shortcut_definition(&with_action(ShortcutAction::Actions(fits.clone()))),
        Ok(())
    );
    let mut over = fits;
    over.push(text(1));
    assert_eq!(
        validate_shortcut_definition(&with_action(ShortcutAction::Actions(over))),
        Err(ShortcutError::TooMuchText)
    );
}

#[test]
fn cue_and_script_targets_name_nonzero_ids() {
    for valid in [
        ShortcutAction::Cue {
            list: 1,
            operation: ShortcutCueOperation::Go { cue: None },
        },
        ShortcutAction::Cue {
            list: 1,
            operation: ShortcutCueOperation::Go { cue: Some(4) },
        },
        ShortcutAction::Cue {
            list: 1,
            operation: ShortcutCueOperation::Reset,
        },
        ShortcutAction::Script { script: 2 },
    ] {
        assert_eq!(validate_shortcut_definition(&with_action(valid)), Ok(()));
    }
    for invalid in [
        ShortcutAction::Cue {
            list: 0,
            operation: ShortcutCueOperation::Stop,
        },
        ShortcutAction::Cue {
            list: 1,
            operation: ShortcutCueOperation::Go { cue: Some(0) },
        },
        ShortcutAction::Script { script: 0 },
    ] {
        assert_eq!(
            validate_shortcut_definition(&with_action(invalid)),
            Err(ShortcutError::InvalidTarget)
        );
    }
}

#[test]
fn the_set_keeps_ids_keys_and_osc_names_unique() {
    assert_eq!(validate_shortcuts(&[shortcut(1), shortcut(2)]), Ok(()));
    assert_eq!(
        validate_shortcuts(&[shortcut(0)]),
        Err(ShortcutError::InvalidId)
    );
    assert_eq!(
        validate_shortcuts(&[shortcut(1), shortcut(1)]),
        Err(ShortcutError::DuplicateId(1))
    );
    let same_key = Shortcut {
        osc: None,
        key: shortcut(1).key,
        ..shortcut(2)
    };
    assert_eq!(
        validate_shortcuts(&[shortcut(1), same_key]),
        Err(ShortcutError::DuplicateKey {
            first: 1,
            second: 2
        })
    );
    // The same key with other modifiers is another key.
    let shifted = Shortcut {
        osc: None,
        key: Some(ShortcutKey {
            shift: true,
            ..ShortcutKey::plain("F1")
        }),
        ..shortcut(2)
    };
    assert_eq!(validate_shortcuts(&[shortcut(1), shifted]), Ok(()));
    let same_osc = Shortcut {
        key: None,
        osc: shortcut(1).osc,
        ..shortcut(2)
    };
    assert_eq!(
        validate_shortcuts(&[shortcut(1), same_osc]),
        Err(ShortcutError::DuplicateOsc {
            first: 1,
            second: 2
        })
    );
    // Shortcuts without a key or OSC name never clash.
    let bare = |id| Shortcut {
        key: None,
        osc: None,
        ..shortcut(id)
    };
    assert_eq!(validate_shortcuts(&[bare(1), bare(2)]), Ok(()));
    let many: Vec<Shortcut> = (1..=u64::try_from(MAX_SHORTCUTS).unwrap() + 1)
        .map(bare)
        .collect();
    assert_eq!(
        validate_shortcuts(&many),
        Err(ShortcutError::TooManyShortcuts)
    );
    assert_eq!(validate_shortcuts(&many[..MAX_SHORTCUTS]), Ok(()));
}

#[test]
fn project_validation_reports_shortcuts() {
    let mut project = project();
    // A shortcut naming an input the project lacks is valid: it is
    // refused when fired, never in the way of removing the input.
    project.set_shortcuts(vec![shortcut(1)]);
    assert!(project.validate().is_ok());
    project.set_shortcuts(vec![shortcut(1), shortcut(1)]);
    let errors = project.validate().unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.field == "shortcuts"
                && error.kind == ValidationErrorKind::DuplicateId),
        "{errors:?}"
    );
}

fn project() -> fm_model::Project {
    use fm_types::{
        AudioFormat, ChannelLayout, ColorMetadata, FrameRate, PixelFormat, ProjectId, SampleFormat,
        SampleRate, ScanMode, VideoDimensions, VideoFormat,
    };
    let frame_rate = FrameRate::new(30, 1).unwrap();
    fm_model::Project::new(
        ProjectId::new(NonZeroU128::MIN),
        "Show",
        fm_model::ProjectSettings {
            frame_rate,
            video: VideoFormat {
                dimensions: VideoDimensions::new(1920, 1080).unwrap(),
                frame_rate,
                pixel_format: PixelFormat::Rgba8,
                scan: ScanMode::Progressive,
                color: ColorMetadata::default(),
            },
            audio: AudioFormat {
                sample_rate: SampleRate::new(48_000).unwrap(),
                sample_format: SampleFormat::F32,
                channels: ChannelLayout::stereo(),
            },
        },
    )
}

/// A chord's second stroke is a plain stroke, a scope is a known panel,
/// and keys clash only where their strokes and scopes overlap.
#[test]
fn chords_and_scopes_are_bounded_and_clash_only_where_they_overlap() {
    let chord = |then: &str, scope: Option<&str>| ShortcutKey {
        command: true,
        then: Some(Box::new(ShortcutKey::plain(then))),
        scope: scope.map(str::to_owned),
        ..ShortcutKey::plain("K")
    };
    assert!(chord("1", Some("cues")).is_valid());
    assert!(!chord("1", Some("timeline")).is_valid());
    assert!(!chord("", None).is_valid());
    let mut nested = chord("1", None);
    nested.then = Some(Box::new(chord("2", None)));
    assert!(!nested.is_valid());
    assert_eq!(chord("1", Some("cues")).to_string(), "Ctrl+K, 1 in cues");

    let with_key = |id: u64, key: ShortcutKey| Shortcut {
        key: Some(key),
        ..shortcut(id)
    };
    assert_eq!(
        validate_shortcuts(&[
            with_key(1, chord("1", None)),
            with_key(2, chord("2", None)),
            with_key(3, chord("1", Some("mixer"))),
        ]),
        Err(ShortcutError::DuplicateKey {
            first: 1,
            second: 3
        })
    );
    assert_eq!(
        validate_shortcuts(&[
            with_key(1, chord("1", Some("cues"))),
            with_key(2, chord("1", Some("mixer"))),
            with_key(
                3,
                ShortcutKey {
                    scope: Some("titles".into()),
                    command: true,
                    ..ShortcutKey::plain("K")
                }
            ),
        ]),
        Ok(())
    );
    assert_eq!(
        validate_shortcuts(&[
            with_key(1, chord("1", None)),
            with_key(
                2,
                ShortcutKey {
                    command: true,
                    ..ShortcutKey::plain("K")
                }
            ),
        ]),
        Err(ShortcutError::DuplicateKey {
            first: 1,
            second: 2
        })
    );
}

/// Schema 75: a key may name a mouse button, and may be scoped to a panel's
/// pop-out window, which overlaps that panel and the whole console but no
/// other panel or window.
#[test]
fn mouse_buttons_and_window_scopes_are_bounded_and_clash_with_keys() {
    let mouse = |name: &str, scope: Option<&str>| ShortcutKey {
        shift: true,
        scope: scope.map(str::to_owned),
        ..ShortcutKey::plain(name)
    };
    for name in fm_model::SHORTCUT_MOUSE_BUTTONS {
        assert!(mouse(name, None).is_valid(), "{name}");
        assert!(mouse(name, None).is_mouse(), "{name}");
    }
    assert!(!ShortcutKey::plain("F9").is_mouse());
    assert!(mouse("MouseBack", Some("mixer-window")).is_valid());
    assert!(mouse("MouseBack", Some("cues-window")).is_valid());
    assert!(!mouse("MouseBack", Some("timeline-window")).is_valid());
    assert!(!mouse("MouseBack", Some("-window")).is_valid());
    assert!(!mouse("MouseBack", Some("mixer-window-window")).is_valid());
    assert_eq!(
        mouse("MouseForward", Some("mixer-window")).to_string(),
        "Shift+MouseForward in mixer-window"
    );

    assert!(fm_model::shortcut_scopes_overlap(
        None,
        Some("mixer-window")
    ));
    assert!(fm_model::shortcut_scopes_overlap(
        Some("mixer"),
        Some("mixer-window")
    ));
    assert!(fm_model::shortcut_scopes_overlap(
        Some("mixer-window"),
        Some("mixer-window")
    ));
    assert!(!fm_model::shortcut_scopes_overlap(
        Some("cues"),
        Some("mixer-window")
    ));
    assert!(!fm_model::shortcut_scopes_overlap(
        Some("cues-window"),
        Some("mixer-window")
    ));

    let with_key = |id: u64, key: ShortcutKey| Shortcut {
        key: Some(key),
        ..shortcut(id)
    };
    // A mouse button clashes like a key: the same button and modifiers in
    // overlapping scopes.
    assert_eq!(
        validate_shortcuts(&[
            with_key(1, mouse("MouseBack", Some("mixer"))),
            with_key(2, mouse("MouseBack", Some("mixer-window"))),
        ]),
        Err(ShortcutError::DuplicateKey {
            first: 1,
            second: 2
        })
    );
    assert_eq!(
        validate_shortcuts(&[
            with_key(1, mouse("MouseBack", Some("cues-window"))),
            with_key(2, mouse("MouseBack", Some("mixer-window"))),
            with_key(3, mouse("MouseForward", None)),
            with_key(
                4,
                ShortcutKey {
                    shift: false,
                    ..mouse("MouseBack", None)
                }
            ),
            with_key(5, ShortcutKey::plain("F9")),
        ]),
        Ok(())
    );
}

/// Schema 80 (RC-004): a mouse button may be either stroke of a chord,
/// clashing as any chord does.
#[test]
fn mouse_buttons_may_be_either_stroke_of_a_chord() {
    let mouse = |name: &str, scope: Option<&str>| ShortcutKey {
        shift: true,
        scope: scope.map(str::to_owned),
        ..ShortcutKey::plain(name)
    };
    let with_key = |id: u64, key: ShortcutKey| Shortcut {
        key: Some(key),
        ..shortcut(id)
    };
    // Schema 80: a mouse button may be a chord's first or second stroke;
    // the second stroke stays plain (no chord, no scope of its own).
    let mut chorded = mouse("MouseBack", None);
    chorded.then = Some(Box::new(ShortcutKey::plain("1")));
    assert!(chorded.is_valid());
    assert_eq!(chorded.to_string(), "Shift+MouseBack, 1");
    let mut ending = ShortcutKey::plain("K");
    ending.then = Some(Box::new(ShortcutKey::plain("MouseBack")));
    assert!(ending.is_valid());
    let mut both = mouse("MouseMiddle", None);
    both.then = Some(Box::new(ShortcutKey::plain("MouseForward")));
    assert!(both.is_valid());
    let mut scoped_then = ShortcutKey::plain("K");
    scoped_then.then = Some(Box::new(mouse("MouseBack", Some("mixer"))));
    assert!(!scoped_then.is_valid());
    // A chord started by a button clashes with that button alone, and
    // two such chords clash only when they end on the same stroke.
    assert_eq!(
        validate_shortcuts(&[
            with_key(1, chorded.clone()),
            with_key(2, mouse("MouseBack", None)),
        ]),
        Err(ShortcutError::DuplicateKey {
            first: 1,
            second: 2
        })
    );
    let mut other = chorded.clone();
    other.then = Some(Box::new(ShortcutKey::plain("MouseMiddle")));
    assert_eq!(
        validate_shortcuts(&[
            with_key(1, chorded.clone()),
            with_key(2, other.clone()),
            with_key(3, ending),
            with_key(4, both),
        ]),
        Ok(())
    );
    assert_eq!(
        validate_shortcuts(&[with_key(1, other.clone()), with_key(2, other)]),
        Err(ShortcutError::DuplicateKey {
            first: 1,
            second: 2
        })
    );
    assert_eq!(
        validate_shortcuts(&[with_key(1, scoped_then)]),
        Err(ShortcutError::InvalidKey)
    );
}

/// RC-004 dynamic values: a shortcut's gain may be fixed, a step, or read
/// from the firing control, each within the strip's range; a mute may
/// toggle; a cue list may step and toggle but never read a control value.
#[test]
fn dynamic_values_are_bounded_and_only_a_shortcut_reads_the_control() {
    use fm_model::{
        Cue, CueGain, CueListError, CueMute, FireValue, InputTarget, validate_cue_list_definition,
    };
    let gain = |gain| CueAction::AudioGain {
        input: InputTarget::Selected,
        gain,
    };
    let fired = gain(CueGain::Fired {
        min: -60_000,
        max: 0,
    });
    for action in [
        fired.clone(),
        gain(CueGain::Step(-3_000)),
        gain(CueGain::Set(24_000)),
        CueAction::AudioMute {
            input: InputTarget::Program,
            mute: CueMute::Toggle,
        },
    ] {
        assert_eq!(
            validate_shortcut_definition(&with_action(ShortcutAction::Actions(vec![
                action.clone()
            ]))),
            Ok(()),
            "{action:?}"
        );
    }
    for bad in [
        CueGain::Set(24_001),
        CueGain::Step(0),
        CueGain::Step(120_001),
        CueGain::Fired { min: 0, max: 0 },
        CueGain::Fired {
            min: -96_001,
            max: 0,
        },
    ] {
        assert_eq!(
            validate_shortcut_definition(&with_action(ShortcutAction::Actions(vec![gain(bad)]))),
            Err(ShortcutError::InvalidAction { action: 0 }),
            "{bad:?}"
        );
    }

    // The fire's value fixes a gain read from the control; nothing else
    // reads it.
    assert!(fired.reads_fire_value());
    assert!(!gain(CueGain::Step(1)).reads_fire_value());
    assert_eq!(fired.with_fire_value(None), None);
    assert_eq!(
        fired.with_fire_value(FireValue::new(500)),
        Some(gain(CueGain::Set(-30_000)))
    );
    let step = gain(CueGain::Step(1));
    assert_eq!(step.with_fire_value(None), Some(step.clone()));

    let cue = |actions| Cue {
        id: 4,
        name: "Fader".into(),
        actions,
        auto_follow: false,
    };
    assert_eq!(
        validate_cue_list_definition(
            "Main",
            &[cue(vec![
                gain(CueGain::Step(-3_000)),
                CueAction::AudioMute {
                    input: InputTarget::Preview,
                    mute: CueMute::Toggle,
                },
            ])]
        ),
        Ok(())
    );
    let refused = validate_cue_list_definition("Main", &[cue(vec![step, fired])]);
    assert_eq!(refused, Err(CueListError::FiredValue { cue: 4, action: 1 }));
    assert!(refused.unwrap_err().to_string().contains("firing control"));
}
