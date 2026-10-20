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
