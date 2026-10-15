//! Typed command refusals (Protocol 2.45) worded in the console language.
//!
//! The daemon's English refusal text is only a fallback: a refusal that
//! carries a [`RefusalReason`] is worded here from its typed parameters, so
//! a Spanish console never shows "a layer source of scene 100".

use fm_protocol::{IdKind, InputUse, RefusalReason};

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
            l.format(
                msg,
                &[(
                    "what",
                    Arg::Str(&l.format(id_kind_msg(*kind), &[("id", Arg::Str(&id.to_string()))])),
                )],
            )
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
            &[("field", Arg::Str(field)), ("max", Arg::Count(*max))],
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
        RefusalReason::NotDurable => plain(Msg::RsnNotDurable),
        RefusalReason::RateLimited => plain(Msg::RsnRateLimited),
        RefusalReason::RuntimeUnsupported => plain(Msg::RsnRuntimeUnsupported),
    }
}

/// A refused command's toast: the typed reason worded in the console
/// language, or the daemon's English `message` when the refusal carries no
/// reason.
#[must_use]
pub fn command_refusal_text(
    l: Locale,
    code: &str,
    message: &str,
    reason: Option<&RefusalReason>,
    names: InputNames<'_>,
) -> String {
    let (code, text) = match reason {
        Some(reason) => (reason.kind(), refusal_reason_text(l, reason, names)),
        None => (code, message.to_owned()),
    };
    l.format(
        Msg::CommandRefused,
        &[("code", Arg::Str(code)), ("reason", Arg::Str(&text))],
    )
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
            command_refusal_text(
                ES,
                "conflict",
                "input 1 cannot be removed",
                Some(&reason),
                &names
            ),
            "Comando rechazado (input_in_use): «Camera 1 Wide» es una fuente de capa de la \
             escena 100. Quite esa capa de la escena primero."
        );
        assert_eq!(
            command_refusal_text(
                Locale::ENGLISH,
                "conflict",
                "input 1 cannot be removed",
                Some(&reason),
                &names
            ),
            "Command refused (input_in_use): \"Camera 1 Wide\" is a layer source of scene 100. \
             Remove that layer from the scene first."
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
    fn a_refusal_without_a_reason_falls_back_to_the_daemon_text() {
        assert_eq!(
            command_refusal_text(
                ES,
                "invalid_command",
                "title field is too long",
                None,
                &names
            ),
            "Comando rechazado (invalid_command): title field is too long"
        );
    }
}
