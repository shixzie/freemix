//! Typed command refusals (Protocol 2.45) worded in the console language.
//!
//! The daemon's English refusal text is only a fallback: a refusal that
//! carries a [`RefusalReason`] is worded here from its typed parameters, so
//! a Spanish console never shows "a layer source of scene 100".

use fm_protocol::{IdKind, InputUse, RefusalReason, ScriptFailureKind};

use super::{Arg, Locale, Msg};

/// Names the inputs a refusal mentions; `None` falls back to
/// "Input <id>" in the console language.
pub type InputNames<'a> = &'a dyn Fn(u128) -> Option<String>;

/// The name of `input`, or its localized fallback name.
fn input_name(l: Locale, names: InputNames<'_>, input: u128) -> String {
    names(input).unwrap_or_else(|| {
        l.format(
            Msg::InputFallbackName,
            &[("input", Arg::Str(&input.to_string()))],
        )
    })
}

/// The localized noun phrase that starts an "unknown" or "duplicate" id
/// sentence.
const fn id_kind_msg(kind: IdKind) -> Msg {
    match kind {
        IdKind::Input => Msg::IdKindInput,
        IdKind::Output => Msg::IdKindOutput,
        IdKind::Scene => Msg::DesignSceneFallback,
        IdKind::Layer => Msg::DesignLayerUnnamed,
        IdKind::Category => Msg::IdKindCategory,
        IdKind::Grade => Msg::IdKindGrade,
        IdKind::Lut => Msg::IdKindLut,
        IdKind::Preset => Msg::IdKindPreset,
        IdKind::Title => Msg::IdKindTitle,
        IdKind::PlaylistEntry => Msg::IdKindPlaylistEntry,
        IdKind::CueList => Msg::IdKindCueList,
        IdKind::Cue => Msg::IdKindCue,
        IdKind::Script => Msg::IdKindScript,
        IdKind::Shortcut => Msg::IdKindShortcut,
    }
}

/// The localized name of a [`RefusalReason::BoundsExceeded`] field token;
/// a token this console does not know passes through as sent.
#[must_use]
pub fn bounds_field_name(l: Locale, field: &str) -> String {
    let msg = match field {
        "inputs" => Msg::BoundsFieldInputs,
        "entries" => Msg::BoundsFieldPlaylistEntries,
        "grades" => Msg::BoundsFieldGrades,
        "luts" => Msg::BoundsFieldLuts,
        "layer_grades" => Msg::BoundsFieldLayerGrades,
        "layers" => Msg::BoundsFieldSceneLayers,
        "effects" => Msg::BoundsFieldLayerEffects,
        "presets" => Msg::BoundsFieldPresets,
        "title_content_bytes" => Msg::BoundsFieldTitleContentBytes,
        "title_text_chars" => Msg::BoundsFieldTitleTextChars,
        "cue_lists" => Msg::BoundsFieldCueLists,
        "cues" => Msg::BoundsFieldCues,
        "actions" => Msg::BoundsFieldCueActions,
        "list_actions" => Msg::BoundsFieldCueListActions,
        "text" => Msg::BoundsFieldCueText,
        "title_number" => Msg::BoundsFieldTitleNumber,
        "scripts" => Msg::BoundsFieldScripts,
        "source" => Msg::BoundsFieldScriptSource,
        other => return other.to_owned(),
    };
    l.text(msg).to_owned()
}

/// The localized phrase naming why a script stopped (RC-011), as it
/// completes "stopped with …".
#[must_use]
pub const fn script_failure_msg(failure: ScriptFailureKind) -> Msg {
    match failure {
        ScriptFailureKind::Syntax => Msg::ScriptFailSyntax,
        ScriptFailureKind::Runtime => Msg::ScriptFailRuntime,
        ScriptFailureKind::InstructionBudget => Msg::ScriptFailInstructions,
        ScriptFailureKind::ActionBudget => Msg::ScriptFailActions,
        ScriptFailureKind::VariableBudget => Msg::ScriptFailVariables,
        ScriptFailureKind::StringBudget => Msg::ScriptFailStringBytes,
        ScriptFailureKind::Refused => Msg::ScriptFailRefused,
        ScriptFailureKind::WaitBudget => Msg::ScriptFailWaits,
        ScriptFailureKind::WaitFramesBudget => Msg::ScriptFailWaitFrames,
    }
}

/// What an operator does about one input usage.
#[must_use]
pub const fn input_use_advice(usage: InputUse) -> Msg {
    match usage {
        InputUse::Program | InputUse::Preview => Msg::AdviceRouteAway,
        InputUse::Transition | InputUse::ManualTransition => Msg::AdviceWaitTransition,
        InputUse::Overlay { .. } | InputUse::OverlayQueue { .. } => Msg::AdviceOverlay,
        InputUse::Stinger { .. } => Msg::AdviceStinger,
        InputUse::SceneLayer { .. } => Msg::AdviceSceneLayer,
        InputUse::PlaylistEntry { .. } => Msg::AdvicePlaylist,
        InputUse::CueAction { .. } => Msg::AdviceCue,
        InputUse::SceneOnAir => Msg::AdviceSceneOffAir,
        InputUse::SceneAudioSource { .. } | InputUse::AliasTarget { .. } => Msg::AdviceOffline,
    }
}

/// The sentence saying how the input `name` is in use.
#[must_use]
pub fn input_use_sentence(
    l: Locale,
    name: &str,
    usage: InputUse,
    input_names: InputNames<'_>,
) -> String {
    let named = |msg: Msg| l.format(msg, &[("name", Arg::Str(name))]);
    let numbered = |msg: Msg, arg: &str, number: &str| {
        l.format(msg, &[("name", Arg::Str(name)), (arg, Arg::Str(number))])
    };
    match usage {
        InputUse::Program => named(Msg::InUseOnProgram),
        InputUse::Preview => named(Msg::InUseOnPreview),
        InputUse::Transition => named(Msg::InUseTransition),
        InputUse::ManualTransition => named(Msg::InUseManualTransition),
        InputUse::SceneOnAir => named(Msg::InUseSceneOnAir),
        InputUse::Overlay { channel } => {
            numbered(Msg::InUseOverlaySource, "channel", &channel.to_string())
        }
        InputUse::OverlayQueue { channel } => {
            numbered(Msg::InUseOverlayQueue, "channel", &channel.to_string())
        }
        InputUse::Stinger { slot } => numbered(Msg::InUseStinger, "slot", &slot.to_string()),
        InputUse::SceneLayer { scene } => {
            numbered(Msg::InUseSceneLayer, "scene", &scene.to_string())
        }
        InputUse::SceneAudioSource { scene_input } => numbered(
            Msg::InUseSceneAudio,
            "scene",
            &input_name(l, input_names, scene_input),
        ),
        InputUse::AliasTarget { alias } => numbered(
            Msg::InUseAliasTarget,
            "alias",
            &input_name(l, input_names, alias),
        ),
        InputUse::PlaylistEntry { entry } => {
            numbered(Msg::InUsePlaylistEntry, "entry", &entry.to_string())
        }
        InputUse::CueAction { list, cue } => l.format(
            Msg::InUseCueAction,
            &[
                ("name", Arg::Str(name)),
                ("cue", Arg::Str(&cue.to_string())),
                ("list", Arg::Str(&list.to_string())),
            ],
        ),
    }
}

/// Words a typed refusal reason in the console language: what happened and,
/// where there is one, what to do next.
#[must_use]
pub fn refusal_reason_text(l: Locale, reason: &RefusalReason, names: InputNames<'_>) -> String {
    let plain = |msg: Msg| l.text(msg).to_owned();
    let number = |msg: Msg, arg: &str, value: u64| l.format(msg, &[(arg, Arg::Plain(value))]);
    match reason {
        RefusalReason::PermissionDenied => plain(Msg::RsnPermissionDenied),
        RefusalReason::DeadlineExceeded => plain(Msg::RsnDeadlineExceeded),
        RefusalReason::RevisionConflict { current } => {
            number(Msg::RsnRevisionConflict, "revision", *current)
        }
        RefusalReason::UnknownId {
            kind: IdKind::Input,
            id,
        } => l.format(
            Msg::RefuseUnknownInput,
            &[("name", Arg::Str(&input_name(l, names, *id)))],
        ),
        RefusalReason::UnknownId { kind, id } | RefusalReason::DuplicateId { kind, id } => {
            let msg = if matches!(reason, RefusalReason::UnknownId { .. }) {
                Msg::RsnUnknownId
            } else {
                Msg::RsnDuplicateId
            };
            // A title is a title input: name it by its input's name.
            let what = match (kind, names(*id)) {
                (IdKind::Title, Some(name)) => {
                    l.format(Msg::IdKindTitleNamed, &[("name", Arg::Str(&name))])
                }
                _ => l.format(id_kind_msg(*kind), &[("id", Arg::Str(&id.to_string()))]),
            };
            l.format(msg, &[("what", Arg::Str(&what))])
        }
        RefusalReason::DuplicateName => plain(Msg::RsnDuplicateName),
        RefusalReason::InvalidName { max_bytes } => {
            number(Msg::RsnInvalidName, "max", u64::from(*max_bytes))
        }
        RefusalReason::ProjectLocked => plain(Msg::RsnProjectLocked),
        RefusalReason::InputLocked { input } => l.format(
            Msg::RsnInputLocked,
            &[("name", Arg::Str(&input_name(l, names, *input)))],
        ),
        RefusalReason::InputInUse { input, usage } => {
            let name = input_name(l, names, *input);
            format!(
                "{} {}",
                input_use_sentence(l, &name, *usage, names),
                l.text(input_use_advice(*usage))
            )
        }
        RefusalReason::DisplayInUse { slot } => {
            number(Msg::RsnDisplayInUse, "slot", u64::from(*slot))
        }
        RefusalReason::BoundsExceeded { field, max } => l.format(
            Msg::RsnBoundsExceeded,
            &[
                ("field", Arg::Str(&bounds_field_name(l, field))),
                ("max", Arg::Count(*max)),
            ],
        ),
        RefusalReason::NothingToUndo => plain(Msg::RsnNothingToUndo),
        RefusalReason::NothingToRedo => plain(Msg::RsnNothingToRedo),
        RefusalReason::HistoryConflict => plain(Msg::RsnHistoryConflict),
        RefusalReason::HistoryDrift => plain(Msg::RsnHistoryDrift),
        RefusalReason::StingerRealizing { slot } => {
            number(Msg::RsnStingerRealizing, "slot", u64::from(*slot))
        }
        RefusalReason::PlaylistTakeScheduled { frame } => {
            number(Msg::RsnPlaylistTakeScheduled, "frame", *frame)
        }
        RefusalReason::CueScheduled { frame } => number(Msg::RsnCueScheduled, "frame", *frame),
        RefusalReason::NoCueOnStandby => plain(Msg::RsnNoCueOnStandby),
        RefusalReason::CueRunning { cue } => number(Msg::RsnCueRunning, "cue", *cue),
        RefusalReason::ScriptRunning { script } => number(Msg::RsnScriptRunning, "script", *script),
        RefusalReason::NotDurable => plain(Msg::RsnNotDurable),
        RefusalReason::RateLimited => plain(Msg::RsnRateLimited),
        RefusalReason::RuntimeUnsupported => plain(Msg::RsnRuntimeUnsupported),
        RefusalReason::ScriptFailed {
            failure,
            line,
            column,
        } => l.format(
            Msg::RsnScriptFailed,
            &[
                ("failure", Arg::Str(l.text(script_failure_msg(*failure)))),
                ("line", Arg::Plain(u64::from(*line))),
                ("column", Arg::Plain(u64::from(*column))),
            ],
        ),
    }
}

/// A refused command's toast: the typed reason worded in the console
/// language, or the daemon's English `message` when the refusal carries no
/// reason. The stable reason code is not part of the sentence; see
/// [`refusal_code_detail`].
#[must_use]
pub fn command_refusal_text(
    l: Locale,
    message: &str,
    reason: Option<&RefusalReason>,
    names: InputNames<'_>,
) -> String {
    let text = match reason {
        Some(reason) => refusal_reason_text(l, reason, names),
        None => message.to_owned(),
    };
    l.format(Msg::CommandRefused, &[("reason", Arg::Str(&text))])
}

/// The hover detail of a refusal toast: its stable reason code
/// (`Reason code: input_in_use`), kept out of the localized sentence but
/// available to support staff.
#[must_use]
pub fn refusal_code_detail(l: Locale, code: &str) -> String {
    l.format(Msg::ToastReasonCode, &[("code", Arg::Str(code))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;

    const ES: Locale = Locale::new(Language::Spanish);

    fn names(input: u128) -> Option<String> {
        (input == 1).then(|| "Camera 1 Wide".to_owned())
    }

    fn every_reason() -> Vec<RefusalReason> {
        let mut reasons = vec![
            RefusalReason::PermissionDenied,
            RefusalReason::DeadlineExceeded,
            RefusalReason::RevisionConflict { current: 9 },
            RefusalReason::DuplicateName,
            RefusalReason::InvalidName { max_bytes: 128 },
            RefusalReason::ProjectLocked,
            RefusalReason::InputLocked { input: 1 },
            RefusalReason::DisplayInUse { slot: 2 },
            RefusalReason::bounds("inputs", 256),
            RefusalReason::NothingToUndo,
            RefusalReason::NothingToRedo,
            RefusalReason::HistoryConflict,
            RefusalReason::HistoryDrift,
            RefusalReason::StingerRealizing { slot: 3 },
            RefusalReason::PlaylistTakeScheduled { frame: 120 },
            RefusalReason::NotDurable,
            RefusalReason::RateLimited,
            RefusalReason::RuntimeUnsupported,
        ];
        for failure in ScriptFailureKind::ALL {
            reasons.push(RefusalReason::ScriptFailed {
                failure,
                line: 4,
                column: 12,
            });
        }
        for kind in IdKind::ALL {
            reasons.push(RefusalReason::UnknownId { kind, id: 4 });
            reasons.push(RefusalReason::DuplicateId { kind, id: 4 });
        }
        for usage in [
            InputUse::Program,
            InputUse::Preview,
            InputUse::Transition,
            InputUse::ManualTransition,
            InputUse::Overlay { channel: 2 },
            InputUse::OverlayQueue { channel: 2 },
            InputUse::Stinger { slot: 3 },
            InputUse::SceneLayer { scene: 100 },
            InputUse::SceneAudioSource { scene_input: 7 },
            InputUse::AliasTarget { alias: 8 },
            InputUse::PlaylistEntry { entry: 5 },
            InputUse::SceneOnAir,
        ] {
            reasons.push(RefusalReason::InputInUse { input: 1, usage });
        }
        reasons
    }

    #[test]
    fn every_reason_is_worded_in_both_languages_without_english_daemon_text() {
        for reason in every_reason() {
            let english = refusal_reason_text(Locale::ENGLISH, &reason, &names);
            let spanish = refusal_reason_text(ES, &reason, &names);
            assert!(!english.is_empty() && !spanish.is_empty(), "{reason:?}");
            assert_ne!(english, spanish, "{reason:?} is untranslated");
            assert!(!spanish.contains('{'), "{spanish}");
            // The daemon's English fallback never leaks into Spanish.
            assert!(
                !spanish.contains(&reason.to_string()),
                "{reason:?}: {spanish}"
            );
        }
    }

    #[test]
    fn scene_layer_refusal_reads_in_spanish() {
        let reason = RefusalReason::InputInUse {
            input: 1,
            usage: InputUse::SceneLayer { scene: 100 },
        };
        assert_eq!(
            command_refusal_text(ES, "input 1 cannot be removed", Some(&reason), &names),
            "Comando rechazado: «Camera 1 Wide» es una fuente de capa de la \
             escena 100. Quite esa capa de la escena primero."
        );
        assert_eq!(
            command_refusal_text(
                Locale::ENGLISH,
                "input 1 cannot be removed",
                Some(&reason),
                &names
            ),
            "Command refused: \"Camera 1 Wide\" is a layer source of scene 100. \
             Remove that layer from the scene first."
        );
        assert_eq!(
            refusal_code_detail(ES, reason.kind()),
            "Código del motivo: input_in_use"
        );
        // An unnamed input gets the localized fallback name.
        let alias = RefusalReason::InputInUse {
            input: 2,
            usage: InputUse::AliasTarget { alias: 9 },
        };
        assert_eq!(
            refusal_reason_text(ES, &alias, &names),
            "«Entrada 2» es el destino del alias «Entrada 9». \
             Quite esa referencia sin conexión primero."
        );
    }

    #[test]
    fn bounds_fields_are_named_in_the_console_language() {
        for field in [
            "inputs",
            "entries",
            "grades",
            "luts",
            "layer_grades",
            "layers",
            "effects",
            "presets",
            "title_content_bytes",
            "title_text_chars",
            "title_number",
        ] {
            let english = bounds_field_name(Locale::ENGLISH, field);
            let spanish = bounds_field_name(ES, field);
            assert_ne!(english, field, "{field} has no English name");
            assert_ne!(spanish, field, "{field} has no Spanish name");
            assert_ne!(english, spanish, "{field} is untranslated");
        }
        let reason = RefusalReason::bounds("layer_grades", 16);
        assert_eq!(
            refusal_reason_text(Locale::ENGLISH, &reason, &names),
            "Layer grades: the limit is 16."
        );
        assert_eq!(
            refusal_reason_text(ES, &reason, &names),
            "Correcciones de capa: el límite es 16."
        );
        // A field this console does not know yet passes through.
        assert_eq!(
            refusal_reason_text(
                Locale::ENGLISH,
                &RefusalReason::bounds("widgets", 3),
                &names
            ),
            "widgets: the limit is 3."
        );
    }

    #[test]
    fn a_title_refusal_names_the_title_when_it_can() {
        let named = RefusalReason::UnknownId {
            kind: IdKind::Title,
            id: 1,
        };
        assert_eq!(
            refusal_reason_text(Locale::ENGLISH, &named, &names),
            "Title \"Camera 1 Wide\" no longer exists."
        );
        assert_eq!(
            refusal_reason_text(ES, &named, &names),
            "Título «Camera 1 Wide» ya no existe."
        );
        let unnamed = RefusalReason::DuplicateId {
            kind: IdKind::Title,
            id: 7,
        };
        assert_eq!(
            refusal_reason_text(Locale::ENGLISH, &unnamed, &names),
            "Title 7 already exists."
        );
    }

    #[test]
    fn a_refusal_without_a_reason_falls_back_to_the_daemon_text() {
        assert_eq!(
            command_refusal_text(ES, "title field is too long", None, &names),
            "Comando rechazado: title field is too long"
        );
    }
}
