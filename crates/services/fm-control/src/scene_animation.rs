//! Wire mapping of the scene animation commands, the
//! `scene_animation_changed` event, and the `scene_animation_state` and
//! `scene_layout_state` records that follow a snapshot (Protocol 2.31,
//! GX-011 stage 5).
//!
//! Scene clips use the title clip encoding with layer ids as targets; a
//! scene's layout lists its layers (names, static boxes) so a remote
//! animation editor can target and outline them.

use fm_engine::{
    Engine, EngineCommand, EngineScene, EngineSceneAnimationEdit, SceneAnimationChange,
    scene_inputs_sharing,
};
use fm_model::fm_animation::{Clip, ClipError, Track, TrackKey};
use fm_model::{LayerId, Rotation, Scene, SceneClip};
use fm_protocol::{
    CommandPayload, EventPayload, MAX_SCENE_ANIMATION_BYTES, MAX_SCENE_CANVAS_SIDE,
    MAX_SCENE_LAYER_NAME_BYTES, MAX_SCENE_LAYOUT_LAYERS, SceneAnimationStateMessage,
    SceneLayoutStateMessage, WireInputId, WireSceneClip, WireSceneLayerId, WireSceneLayerOutline,
    WireSceneLayout, scene_clip_wire_bytes,
};
use fm_types::InputId;

/// `clip` with every layer id carried as a wire id. Ids keep their order,
/// so the tracks stay sorted.
///
/// # Panics
///
/// Never: the mapping keeps every id, so the clip stays valid.
#[must_use]
pub fn scene_clip_to_wire(clip: &SceneClip) -> WireSceneClip {
    retarget(clip, |layer| WireSceneLayerId::new(layer.non_zero()))
        .expect("an id-preserving retarget of a valid clip is valid")
}

/// Whether `clip`'s wire encoding fits [`MAX_SCENE_ANIMATION_BYTES`], the
/// bound every scene animation edit is held to.
#[must_use]
pub fn scene_clip_fits_wire(clip: &SceneClip) -> bool {
    scene_clip_wire_bytes(&scene_clip_to_wire(clip)) <= MAX_SCENE_ANIMATION_BYTES
}

/// The domain clip a wire clip names.
///
/// # Errors
///
/// None in practice: wire clips are validated by the codec and the mapping
/// keeps ids and their order; the error is kept for exactness.
pub fn scene_clip_from_wire(clip: &WireSceneClip) -> Result<SceneClip, ClipError<LayerId>> {
    retarget(clip, |layer| LayerId::from_non_zero(layer.get()))
}

fn retarget<A: Copy + Ord, B: Copy + Ord>(
    clip: &Clip<A>,
    map: impl Fn(A) -> B,
) -> Result<Clip<B>, ClipError<B>> {
    Clip::from_tracks(
        clip.timebase(),
        clip.in_frames(),
        clip.out_frames(),
        clip.tracks()
            .iter()
            .map(|track| {
                Track::new(
                    TrackKey::new(map(track.key.target), track.key.phase, track.key.property),
                    track.keys.clone(),
                )
            })
            .collect(),
    )
}

/// Maps the scene animation commands. Layer ids map one to one, so a wire
/// clip the codec accepted maps exactly.
pub fn scene_animation_engine_command(payload: &CommandPayload) -> EngineCommand {
    let (input, base_revision, change) = match payload {
        CommandPayload::SetSceneAnimation {
            input,
            base_revision,
            clip,
        } => (
            input,
            base_revision,
            SceneAnimationChange::Set(Box::new(
                scene_clip_from_wire(clip)
                    .expect("an id-preserving retarget of a valid clip is valid"),
            )),
        ),
        CommandPayload::ApplySceneAnimationPreset {
            input,
            base_revision,
            layer,
            phase,
            preset,
            frames,
        } => (
            input,
            base_revision,
            SceneAnimationChange::Preset {
                layer: LayerId::from_non_zero(layer.get()),
                phase: *phase,
                preset: *preset,
                frames: *frames,
            },
        ),
        CommandPayload::StretchSceneAnimation {
            input,
            base_revision,
            phase,
            frames,
        } => (
            input,
            base_revision,
            SceneAnimationChange::Stretch {
                phase: *phase,
                frames: *frames,
            },
        ),
        _ => unreachable!("only scene animation commands are delegated"),
    };
    EngineCommand::SceneAnimation(EngineSceneAnimationEdit {
        input: input.to_domain(),
        base_revision: Some(*base_revision),
        change,
    })
}

/// The history target an undo or redo would apply, with the animation
/// revision it applies at (see `title_animation::checked_history_target`).
pub fn checked_history_target(engine: &Engine, command: EngineCommand) -> EngineCommand {
    match command {
        EngineCommand::SceneAnimation(edit) if edit.base_revision.is_none() => {
            let base_revision = engine
                .show()
                .scene_input(edit.input)
                .map(|scene| scene.animation().revision);
            EngineCommand::SceneAnimation(EngineSceneAnimationEdit {
                base_revision,
                ..edit
            })
        }
        command => command,
    }
}

/// Maps an undoable scene animation edit back to its exact wire payload;
/// `None` without a base revision.
pub fn wire_scene_animation_payload(edit: &EngineSceneAnimationEdit) -> Option<CommandPayload> {
    let base_revision = edit.base_revision?;
    let input = WireInputId::from_domain(edit.input);
    Some(match &edit.change {
        SceneAnimationChange::Set(clip) => CommandPayload::SetSceneAnimation {
            input,
            base_revision,
            clip: Box::new(scene_clip_to_wire(clip)),
        },
        SceneAnimationChange::Preset {
            layer,
            phase,
            preset,
            frames,
        } => CommandPayload::ApplySceneAnimationPreset {
            input,
            base_revision,
            layer: WireSceneLayerId::new(layer.non_zero()),
            phase: *phase,
            preset: *preset,
            frames: *frames,
        },
        SceneAnimationChange::Stretch { phase, frames } => CommandPayload::StretchSceneAnimation {
            input,
            base_revision,
            phase: *phase,
            frames: *frames,
        },
    })
}

/// The `scene_animation_changed` payload for an accepted edit through
/// `input` in the post-command `engine`: every Scene input showing the
/// scene and the animation they all show.
pub fn scene_animation_changed(engine: &Engine, input: InputId) -> EventPayload {
    let scene = engine
        .show()
        .scene_input(input)
        .expect("an accepted scene animation edit targets a Scene input");
    EventPayload::SceneAnimationChanged {
        inputs: scene_inputs_sharing(engine.show(), input)
            .into_iter()
            .map(WireInputId::from_domain)
            .collect(),
        animation_revision: scene.animation().revision,
        clip: Box::new(scene_clip_to_wire(&scene.animation().clip)),
    }
}

/// Every Scene input in ascending order: a snapshot's `scenes` list.
pub fn snapshot_scenes(engine: &Engine) -> Vec<WireInputId> {
    engine
        .show()
        .scene_inputs()
        .keys()
        .take(fm_protocol::MAX_LIST_ITEMS)
        .copied()
        .map(WireInputId::from_domain)
        .collect()
}

fn announced(engine: &Engine) -> impl Iterator<Item = (InputId, &EngineScene)> {
    engine
        .show()
        .scene_inputs()
        .keys()
        .take(fm_protocol::MAX_LIST_ITEMS)
        .filter_map(|input| Some((*input, engine.show().scene_input(*input)?)))
}

/// One `scene_animation_state` per announced Scene input, in snapshot
/// order.
pub fn scene_animation_states(engine: &Engine) -> Vec<SceneAnimationStateMessage> {
    let revision = engine.revision().get();
    announced(engine)
        .map(|(input, scene)| SceneAnimationStateMessage {
            revision,
            input: WireInputId::from_domain(input),
            animation_revision: scene.animation().revision,
            clip: Box::new(scene_clip_to_wire(&scene.animation().clip)),
        })
        .collect()
}

/// One `scene_layout_state` per announced Scene input, in snapshot order.
pub fn scene_layout_states(engine: &Engine) -> Vec<SceneLayoutStateMessage> {
    let revision = engine.revision().get();
    announced(engine)
        .map(|(input, scene)| SceneLayoutStateMessage {
            revision,
            input: WireInputId::from_domain(input),
            layout: Box::new(scene_layout_to_wire(
                &scene.scene,
                scene.canvas_width,
                scene.canvas_height,
            )),
        })
        .collect()
}

/// `name` as a wire display name: control characters become spaces and the
/// result is cut at a character boundary within
/// [`MAX_SCENE_LAYER_NAME_BYTES`].
fn display_name(name: &str) -> String {
    let mut out = String::new();
    for character in name.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if out.len() + character.len_utf8() > MAX_SCENE_LAYER_NAME_BYTES {
            break;
        }
        out.push(character);
    }
    out
}

/// `scene`'s wire layout on a `canvas_width` x `canvas_height` canvas: its
/// first [`MAX_SCENE_LAYOUT_LAYERS`] layers in scene order, each with its id,
/// display name, static footprint origin and pre-rotation size, quarter
/// turns, opacity, and enablement. Canvas sides are clamped to
/// `1..=`[`MAX_SCENE_CANVAS_SIDE`].
#[must_use]
pub fn scene_layout_to_wire(
    scene: &Scene,
    canvas_width: u32,
    canvas_height: u32,
) -> WireSceneLayout {
    let side = |value: u32| value.clamp(1, MAX_SCENE_CANVAS_SIDE);
    WireSceneLayout {
        width: side(canvas_width),
        height: side(canvas_height),
        layers: scene
            .layers
            .iter()
            .take(MAX_SCENE_LAYOUT_LAYERS)
            .map(|layer| WireSceneLayerOutline {
                id: WireSceneLayerId::new(layer.id.non_zero()),
                name: display_name(&layer.name),
                x: layer.geometry.translation_x,
                y: layer.geometry.translation_y,
                width: layer.geometry.width,
                height: layer.geometry.height,
                quarter_turns: match layer.geometry.rotation {
                    Rotation::Deg0 => 0,
                    Rotation::Deg90 => 1,
                    Rotation::Deg180 => 2,
                    Rotation::Deg270 => 3,
                },
                opacity: layer.opacity,
                enabled: layer.enabled,
            })
            .collect(),
    }
}
