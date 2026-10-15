//! Wire mapping of the animation preset library (Protocol 2.34): the
//! `animation_preset_*` commands, the `animation_preset_changed` event, and
//! the `animation_preset_state` records that follow a snapshot.

use fm_engine::{Engine, EngineAnimationPresetEdit, EngineCommand};
use fm_model::{AnimationPresetId, UserAnimationPreset};
use fm_protocol::{
    AnimationPresetChange, AnimationPresetEntry, AnimationPresetStateMessage, CommandPayload,
    EventPayload,
};

// The wire carries exactly the library the project holds, so the protocol's
// bounds must be the model's bounds, and every motion the model accepts must
// have a wire encoding: at most 25 bytes a key (frame, value, bezier
// easing), a few bytes a track, and the reference box and clip header.
const _: () = {
    assert!(fm_protocol::MAX_ANIMATION_PRESETS as usize == fm_model::MAX_PROJECT_ANIMATION_PRESETS);
    assert!(
        fm_protocol::MAX_ANIMATION_PRESET_NAME_BYTES == fm_model::MAX_ANIMATION_PRESET_NAME_BYTES
    );
    assert!(
        fm_model::fm_animation::MAX_MOTION_KEYS * 25
            + fm_model::fm_animation::Property::ALL.len() * 8
            + 64
            <= fm_protocol::MAX_ANIMATION_PRESET_BYTES
    );
};

fn entry(preset: &UserAnimationPreset) -> AnimationPresetEntry {
    AnimationPresetEntry {
        preset: preset.id.get(),
        name: preset.name.clone(),
        phase: preset.phase,
        motion: Box::new(preset.motion.clone()),
    }
}

/// Maps the animation preset library commands.
pub fn animation_preset_engine_command(payload: &CommandPayload) -> EngineCommand {
    EngineCommand::AnimationPreset(match payload {
        CommandPayload::AddAnimationPreset {
            preset,
            name,
            phase,
            motion,
            index,
        } => EngineAnimationPresetEdit::Add {
            preset: Box::new(UserAnimationPreset {
                id: AnimationPresetId::new(*preset),
                name: name.clone(),
                phase: *phase,
                motion: (**motion).clone(),
            }),
            index: index.map(usize::from),
        },
        CommandPayload::RenameAnimationPreset { preset, name } => {
            EngineAnimationPresetEdit::Rename {
                id: AnimationPresetId::new(*preset),
                name: name.clone(),
            }
        }
        CommandPayload::RemoveAnimationPreset { preset } => EngineAnimationPresetEdit::Remove {
            id: AnimationPresetId::new(*preset),
        },
        _ => unreachable!("only animation preset library commands are delegated"),
    })
}

/// Maps an undoable library edit back to its exact wire payload.
pub fn wire_animation_preset_payload(edit: &EngineAnimationPresetEdit) -> CommandPayload {
    match edit {
        EngineAnimationPresetEdit::Add { preset, index } => CommandPayload::AddAnimationPreset {
            preset: preset.id.get(),
            name: preset.name.clone(),
            phase: preset.phase,
            motion: Box::new(preset.motion.clone()),
            index: index.and_then(|index| u16::try_from(index).ok()),
        },
        EngineAnimationPresetEdit::Rename { id, name } => CommandPayload::RenameAnimationPreset {
            preset: id.get(),
            name: name.clone(),
        },
        EngineAnimationPresetEdit::Remove { id } => {
            CommandPayload::RemoveAnimationPreset { preset: id.get() }
        }
    }
}

/// The `animation_preset_changed` payload for an accepted `edit` in the
/// post-command `engine`.
pub fn animation_preset_changed(engine: &Engine, edit: &EngineAnimationPresetEdit) -> EventPayload {
    let library = engine.show().animation_presets();
    EventPayload::AnimationPresetChanged(match edit {
        EngineAnimationPresetEdit::Add { .. } | EngineAnimationPresetEdit::Rename { .. } => {
            let index = library
                .index_of(edit.id())
                .expect("an accepted add or rename leaves the preset in the library");
            AnimationPresetChange::Set {
                index: u16::try_from(index).expect("the library is bounded"),
                entry: entry(&library.presets()[index]),
            }
        }
        EngineAnimationPresetEdit::Remove { id } => {
            AnimationPresetChange::Removed { preset: id.get() }
        }
    })
}

/// Every library preset of `engine`, in library order, as the
/// `animation_preset_state` records that follow a snapshot.
pub fn animation_preset_states(engine: &Engine) -> Vec<AnimationPresetStateMessage> {
    let revision = engine.revision().get();
    engine
        .show()
        .animation_presets()
        .presets()
        .iter()
        .map(|preset| AnimationPresetStateMessage {
            revision,
            entry: entry(preset),
        })
        .collect()
}

/// How many `animation_preset_state` records follow a snapshot of `engine`.
pub fn animation_preset_state_count(engine: &Engine) -> u16 {
    u16::try_from(engine.show().animation_presets().len()).expect("the library is bounded")
}
