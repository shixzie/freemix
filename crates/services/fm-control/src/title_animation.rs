//! Wire mapping of the title animation commands, the
//! `title_animation_changed` event, and the `title_animation_state` records
//! that follow a snapshot (Protocol 2.29, GX-011 stage 4a).

use fm_engine::{
    Engine, EngineCommand, EngineTitleAnimationEdit, TitleAnimationChange, title_template_inputs,
};
use fm_protocol::{
    CommandPayload, EventPayload, MAX_TITLE_ANIMATION_BYTES, TitleAnimationStateMessage,
    WireInputId, WireTitleClip, WireTitleElementId, title_clip_wire_bytes,
};
use fm_titles::fm_animation::{Clip, ClipError, Track, TrackKey};
use fm_titles::{ElementId, TitleAnimation, TitleClip};
use fm_types::InputId;

/// `clip` with every element id carried as a wire id. Ids keep their order,
/// so the tracks stay sorted.
///
/// # Panics
///
/// Never: the mapping keeps every id, so the clip stays valid.
#[must_use]
pub fn title_clip_to_wire(clip: &TitleClip) -> WireTitleClip {
    retarget(clip, |element| WireTitleElementId::new(element.get()))
        .expect("an id-preserving retarget of a valid clip is valid")
}

/// Whether `clip`'s wire encoding fits [`MAX_TITLE_ANIMATION_BYTES`], the
/// bound every title animation edit is held to.
#[must_use]
pub fn title_clip_fits_wire(clip: &TitleClip) -> bool {
    title_clip_wire_bytes(&title_clip_to_wire(clip)) <= MAX_TITLE_ANIMATION_BYTES
}

/// The domain clip a wire clip names.
///
/// # Errors
///
/// None in practice: wire clips are validated by the codec and the mapping
/// keeps ids and their order; the error is kept for exactness.
pub fn title_clip_from_wire(clip: &WireTitleClip) -> Result<TitleClip, ClipError<ElementId>> {
    retarget(clip, |element| ElementId::new(element.get()))
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

/// The animation `input`'s title shows: its template's, or the empty static
/// animation at revision zero for a title restored without a template (which
/// refuses every edit).
fn title_animation(engine: &Engine, input: InputId) -> Option<TitleAnimation> {
    let title = engine.show().title(input)?;
    Some(title.animation().cloned().unwrap_or_default())
}

/// Maps the title animation commands. Element ids map one to one, so a
/// wire clip the codec accepted maps exactly.
pub fn title_animation_engine_command(payload: &CommandPayload) -> EngineCommand {
    let (input, base_revision, change) = match payload {
        CommandPayload::SetTitleAnimation {
            input,
            base_revision,
            clip,
        } => (
            input,
            base_revision,
            TitleAnimationChange::Set(Box::new(
                title_clip_from_wire(clip)
                    .expect("an id-preserving retarget of a valid clip is valid"),
            )),
        ),
        CommandPayload::ApplyTitleAnimationPreset {
            input,
            base_revision,
            element,
            phase,
            preset,
            frames,
        } => (
            input,
            base_revision,
            TitleAnimationChange::Preset {
                element: ElementId::new(element.get()),
                phase: *phase,
                preset: *preset,
                frames: *frames,
            },
        ),
        CommandPayload::StretchTitleAnimation {
            input,
            base_revision,
            phase,
            frames,
        } => (
            input,
            base_revision,
            TitleAnimationChange::Stretch {
                phase: *phase,
                frames: *frames,
            },
        ),
        _ => unreachable!("only title animation commands are delegated"),
    };
    EngineCommand::TitleAnimation(EngineTitleAnimationEdit {
        input: input.to_domain(),
        base_revision: Some(*base_revision),
        change,
    })
}

/// The history target an undo or redo would apply, with the animation
/// revision it applies at, so its replayable wire form is revision-checked
/// like any client edit.
pub fn checked_history_target(engine: &Engine, command: EngineCommand) -> EngineCommand {
    match command {
        EngineCommand::TitleAnimation(edit) if edit.base_revision.is_none() => {
            let base_revision = engine
                .show()
                .title(edit.input)
                .and_then(|title| title.animation())
                .map(|animation| animation.revision);
            EngineCommand::TitleAnimation(EngineTitleAnimationEdit {
                base_revision,
                ..edit
            })
        }
        command => command,
    }
}

/// Maps an undoable animation edit (always a whole clip in history) back to
/// its exact wire payload; `None` without a base revision.
pub fn wire_title_animation_payload(edit: &EngineTitleAnimationEdit) -> Option<CommandPayload> {
    let base_revision = edit.base_revision?;
    let input = WireInputId::from_domain(edit.input);
    Some(match &edit.change {
        TitleAnimationChange::Set(clip) => CommandPayload::SetTitleAnimation {
            input,
            base_revision,
            clip: Box::new(title_clip_to_wire(clip)),
        },
        TitleAnimationChange::Preset {
            element,
            phase,
            preset,
            frames,
        } => CommandPayload::ApplyTitleAnimationPreset {
            input,
            base_revision,
            element: WireTitleElementId::new(element.get()),
            phase: *phase,
            preset: *preset,
            frames: *frames,
        },
        TitleAnimationChange::Stretch { phase, frames } => CommandPayload::StretchTitleAnimation {
            input,
            base_revision,
            phase: *phase,
            frames: *frames,
        },
    })
}

/// The `title_animation_changed` payload for an accepted edit of `input`'s
/// template in the post-command `engine`: every input sharing the template
/// and the animation they all show.
pub fn title_animation_changed(engine: &Engine, input: InputId) -> EventPayload {
    let animation =
        title_animation(engine, input).expect("an accepted animation edit targets a title");
    EventPayload::TitleAnimationChanged {
        inputs: title_template_inputs(engine.show(), input)
            .into_iter()
            .map(WireInputId::from_domain)
            .collect(),
        animation_revision: animation.revision,
        clip: Box::new(title_clip_to_wire(&animation.clip)),
    }
}

/// One `title_animation_state` per announced title, in snapshot order.
pub fn title_animation_states(engine: &Engine) -> Vec<TitleAnimationStateMessage> {
    let revision = engine.revision().get();
    engine
        .show()
        .titles()
        .iter()
        .map(|(input, title)| {
            let animation = title.animation().cloned().unwrap_or_default();
            TitleAnimationStateMessage {
                revision,
                input: WireInputId::from_domain(*input),
                animation_revision: animation.revision,
                clip: Box::new(title_clip_to_wire(&animation.clip)),
            }
        })
        .collect()
}
