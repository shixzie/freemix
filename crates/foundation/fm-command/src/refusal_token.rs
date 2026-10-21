//! The stable token form of a [`RefusalReason`] (Protocol 2.45), shared by
//! the wire `reason` field and the faults deferred automation keeps
//! (Protocol 2.63, schema 57).
//!
//! The token is one colon-separated list, the kind first and then its typed
//! parameters, every number a canonical unsigned decimal (slots and
//! channels nonzero); `fm-protocol` documents every form.

use crate::{IdKind, InputUse, RefusalReason, ScriptFailureKind};

/// Parses a canonical unsigned decimal: digits only, no leading zero.
fn canonical_u64(value: &str) -> Option<u64> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

const fn kind_token(kind: IdKind) -> &'static str {
    match kind {
        IdKind::Input => "input",
        IdKind::Output => "output",
        IdKind::Scene => "scene",
        IdKind::Layer => "layer",
        IdKind::Category => "category",
        IdKind::Grade => "grade",
        IdKind::Lut => "lut",
        IdKind::Preset => "preset",
        IdKind::Title => "title",
        IdKind::PlaylistEntry => "playlist_entry",
        IdKind::CueList => "cue_list",
        IdKind::Cue => "cue",
        IdKind::Script => "script",
        IdKind::Shortcut => "shortcut",
    }
}

fn parse_kind(token: &str) -> Option<IdKind> {
    IdKind::ALL
        .into_iter()
        .find(|kind| kind_token(*kind) == token)
}

fn usage_token(usage: InputUse) -> String {
    match usage {
        InputUse::Program => "program".to_owned(),
        InputUse::Preview => "preview".to_owned(),
        InputUse::Transition => "transition".to_owned(),
        InputUse::ManualTransition => "manual_transition".to_owned(),
        InputUse::SceneOnAir => "scene_on_air".to_owned(),
        InputUse::ReplaySource => "replay_source".to_owned(),
        InputUse::Overlay { channel } => format!("overlay:{channel}"),
        InputUse::OverlayQueue { channel } => format!("overlay_queue:{channel}"),
        InputUse::Stinger { slot } => format!("stinger:{slot}"),
        InputUse::SceneLayer { scene } => format!("scene_layer:{scene}"),
        InputUse::SceneAudioSource { scene_input } => format!("scene_audio_source:{scene_input}"),
        InputUse::AliasTarget { alias } => format!("alias_target:{alias}"),
        InputUse::PlaylistEntry { entry } => format!("playlist_entry:{entry}"),
        InputUse::CueAction { list, cue } => format!("cue_action:{list}:{cue}"),
    }
}

fn parse_u128(value: &str) -> Option<u128> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

fn parse_nonzero_u8(value: &str) -> Option<u8> {
    canonical_u64(value)
        .and_then(|value| u8::try_from(value).ok())
        .filter(|value| *value != 0)
}

fn parse_usage(parts: &[&str]) -> Option<InputUse> {
    Some(match parts {
        ["program"] => InputUse::Program,
        ["preview"] => InputUse::Preview,
        ["transition"] => InputUse::Transition,
        ["manual_transition"] => InputUse::ManualTransition,
        ["scene_on_air"] => InputUse::SceneOnAir,
        ["replay_source"] => InputUse::ReplaySource,
        ["overlay", channel] => InputUse::Overlay {
            channel: parse_nonzero_u8(channel)?,
        },
        ["overlay_queue", channel] => InputUse::OverlayQueue {
            channel: parse_nonzero_u8(channel)?,
        },
        ["stinger", slot] => InputUse::Stinger {
            slot: parse_nonzero_u8(slot)?,
        },
        ["scene_layer", scene] => InputUse::SceneLayer {
            scene: parse_u128(scene)?,
        },
        ["scene_audio_source", input] => InputUse::SceneAudioSource {
            scene_input: parse_u128(input)?,
        },
        ["alias_target", alias] => InputUse::AliasTarget {
            alias: parse_u128(alias)?,
        },
        ["playlist_entry", entry] => InputUse::PlaylistEntry {
            entry: canonical_u64(entry)?,
        },
        ["cue_action", list, cue] => InputUse::CueAction {
            list: canonical_u64(list)?,
            cue: canonical_u64(cue)?,
        },
        _ => return None,
    })
}

impl RefusalReason {
    /// The reason's stable token, or `None` for a reason no token holds (a
    /// zero slot or channel, a zero script, a zero line or column, or a
    /// malformed bounds field).
    #[must_use]
    pub fn token(&self) -> Option<String> {
        let nonzero = |value: u8| (value != 0).then_some(value);
        let parameters = match self {
            Self::PermissionDenied
            | Self::DeadlineExceeded
            | Self::DuplicateName
            | Self::ProjectLocked
            | Self::NothingToUndo
            | Self::NothingToRedo
            | Self::HistoryConflict
            | Self::HistoryDrift
            | Self::NotDurable
            | Self::RateLimited
            | Self::RuntimeUnsupported
            | Self::NoCueOnStandby => None,
            Self::RevisionConflict { current } => Some(current.to_string()),
            Self::UnknownId { kind, id } | Self::DuplicateId { kind, id } => {
                Some(format!("{}:{id}", kind_token(*kind)))
            }
            Self::InvalidName { max_bytes } => Some(max_bytes.to_string()),
            Self::InputLocked { input } => Some(input.to_string()),
            Self::InputInUse { input, usage } => {
                if let InputUse::Overlay { channel }
                | InputUse::OverlayQueue { channel }
                | InputUse::Stinger { slot: channel } = usage
                {
                    nonzero(*channel)?;
                }
                Some(format!("{input}:{}", usage_token(*usage)))
            }
            Self::DisplayInUse { slot } | Self::StingerRealizing { slot } => {
                Some(nonzero(*slot)?.to_string())
            }
            Self::BoundsExceeded { field, max } => {
                if !Self::field_is_valid(field) {
                    return None;
                }
                Some(format!("{field}:{max}"))
            }
            Self::PlaylistTakeScheduled { frame } | Self::CueScheduled { frame } => {
                Some(frame.to_string())
            }
            Self::CueRunning { cue } => Some(cue.to_string()),
            Self::ScriptRunning { script } => {
                if *script == 0 {
                    return None;
                }
                Some(script.to_string())
            }
            Self::ScriptFailed {
                failure,
                line,
                column,
            } => {
                if *line == 0 || *column == 0 {
                    return None;
                }
                Some(format!("{}:{line}:{column}", failure.token()))
            }
        };
        Some(match parameters {
            Some(parameters) => format!("{}:{parameters}", self.kind()),
            None => self.kind().to_owned(),
        })
    }

    /// The reason a token names, the exact inverse of [`Self::token`]:
    /// `None` for an unknown kind or a missing, extra, or non-canonical
    /// parameter.
    #[must_use]
    pub fn from_token(value: &str) -> Option<Self> {
        let parts = value.split(':').take(5).collect::<Vec<_>>();
        match parts.as_slice() {
            ["permission_denied"] => Some(Self::PermissionDenied),
            ["deadline_exceeded"] => Some(Self::DeadlineExceeded),
            ["duplicate_name"] => Some(Self::DuplicateName),
            ["project_locked"] => Some(Self::ProjectLocked),
            ["nothing_to_undo"] => Some(Self::NothingToUndo),
            ["nothing_to_redo"] => Some(Self::NothingToRedo),
            ["history_conflict"] => Some(Self::HistoryConflict),
            ["history_drift"] => Some(Self::HistoryDrift),
            ["not_durable"] => Some(Self::NotDurable),
            ["rate_limited"] => Some(Self::RateLimited),
            ["runtime_unsupported"] => Some(Self::RuntimeUnsupported),
            ["no_cue_on_standby"] => Some(Self::NoCueOnStandby),
            ["cue_scheduled", frame] => {
                canonical_u64(frame).map(|frame| Self::CueScheduled { frame })
            }
            ["cue_running", cue] => canonical_u64(cue).map(|cue| Self::CueRunning { cue }),
            ["script_running", script] => canonical_u64(script)
                .filter(|script| *script != 0)
                .map(|script| Self::ScriptRunning { script }),
            ["script_failed", failure, line, column] => {
                let position = |value: &str| {
                    canonical_u64(value)
                        .and_then(|value| u32::try_from(value).ok())
                        .filter(|value| *value != 0)
                };
                ScriptFailureKind::from_token(failure)
                    .zip(position(line))
                    .zip(position(column))
                    .map(|((failure, line), column)| Self::ScriptFailed {
                        failure,
                        line,
                        column,
                    })
            }
            ["revision_conflict", current] => {
                canonical_u64(current).map(|current| Self::RevisionConflict { current })
            }
            ["unknown_id", kind, id] => parse_kind(kind)
                .zip(parse_u128(id))
                .map(|(kind, id)| Self::UnknownId { kind, id }),
            ["duplicate_id", kind, id] => parse_kind(kind)
                .zip(parse_u128(id))
                .map(|(kind, id)| Self::DuplicateId { kind, id }),
            ["invalid_name", max] => canonical_u64(max)
                .and_then(|max| u32::try_from(max).ok())
                .map(|max_bytes| Self::InvalidName { max_bytes }),
            ["input_locked", input] => parse_u128(input).map(|input| Self::InputLocked { input }),
            ["input_in_use", input, usage @ ..] => parse_u128(input)
                .zip(parse_usage(usage))
                .map(|(input, usage)| Self::InputInUse { input, usage }),
            ["display_in_use", slot] => {
                parse_nonzero_u8(slot).map(|slot| Self::DisplayInUse { slot })
            }
            ["bounds_exceeded", field, max] if Self::field_is_valid(field) => canonical_u64(max)
                .map(|max| Self::BoundsExceeded {
                    field: (*field).to_owned(),
                    max,
                }),
            ["stinger_realizing", slot] => {
                parse_nonzero_u8(slot).map(|slot| Self::StingerRealizing { slot })
            }
            ["playlist_take_scheduled", frame] => {
                canonical_u64(frame).map(|frame| Self::PlaylistTakeScheduled { frame })
            }
            _ => None,
        }
    }
}
