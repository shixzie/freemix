//! Title animation edits (GX-011): a title template's keyframe animation
//! changed through the engine's command path.
//!
//! A title input restored with its template ([`EngineTitle::with_template`])
//! carries the whole template behind a shared key (its stored URI): every
//! title input naming the same key shows one animation, so an edit through
//! any of them changes all of them. Each [`EngineTitleAnimationEdit`] is a
//! project edit:
//!
//! * revision-checked: a command from a client names the animation revision
//!   it was based on and is refused as a conflict on any other revision;
//! * padlocked: the project padlock and the padlock of every title input
//!   sharing the template cover it;
//! * undoable: the history records the edit as the exact clip it produced
//!   and its inverse as the exact clip it replaced. Undo and redo carry no
//!   base revision; the history's drift check (the clip must still be the
//!   one the entry recorded) stands in for it. Every applied edit, undo and
//!   redo included, advances the animation revision by one.
//!
//! The In and Out phase lengths a title take plays are read from the
//! template, so an accepted edit changes what the next take or off-air
//! plays; a phase already playing keeps the length it started with.
//!
//! [`EngineTitle::with_template`]: crate::EngineTitle::with_template

use std::sync::Arc;

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_titles::{
    AnimationEditError, ElementId, FieldId, TitleAirTiming, TitleAnimation, TitleClip,
    TitleTemplate,
    fm_animation::{AnimationPreset, ClipError, Phase},
};
use fm_types::InputId;

use crate::ShowState;

/// The template a title input renders and animates, shared by every title
/// input whose `key` (the stored template URI) is the same.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineTitleTemplate {
    pub key: Arc<str>,
    pub template: Arc<TitleTemplate>,
}

impl EngineTitleTemplate {
    #[must_use]
    pub fn new(key: impl Into<Arc<str>>, template: TitleTemplate) -> Self {
        Self {
            key: key.into(),
            template: Arc::new(template),
        }
    }

    /// The template's animation and its revision.
    #[must_use]
    pub fn animation(&self) -> &TitleAnimation {
        &self.template.animation
    }

    /// The In and Out phase lengths a take and an off-air play.
    #[must_use]
    pub fn timing(&self) -> TitleAirTiming {
        TitleAirTiming::of(&self.template.animation)
    }
}

/// Whether a title clip may be the result of an animation edit (see
/// [`crate::Engine::set_title_clip_bound`]).
pub type TitleClipBound = fn(&TitleClip) -> bool;

/// What one title animation edit changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleAnimationChange {
    /// Replaces the whole clip.
    Set(Box<TitleClip>),
    /// Applies a built-in preset to one element over `frames` frames of
    /// `phase` (see [`TitleTemplate::apply_animation_preset`]).
    Preset {
        element: ElementId,
        phase: Phase,
        preset: AnimationPreset,
        frames: u32,
    },
    /// Scales every key of `phase` to `frames` frames (see
    /// [`TitleTemplate::stretch_animation`]).
    Stretch { phase: Phase, frames: u32 },
    /// Applies library preset `preset` to one element over `frames` frames
    /// of `phase` (see [`TitleTemplate::apply_animation_motion`]).
    Motion {
        element: ElementId,
        phase: Phase,
        preset: fm_model::AnimationPresetId,
        frames: u32,
    },
}

/// One edit of the animation of `input`'s template. A project edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineTitleAnimationEdit {
    pub input: InputId,
    /// The animation revision the edit was based on. `None` only for the
    /// clips undo and redo apply.
    pub base_revision: Option<u64>,
    pub change: TitleAnimationChange,
}

impl EngineTitleAnimationEdit {
    /// The same edit with no base revision, as the history stores it.
    #[must_use]
    pub(crate) fn unchecked(&self) -> Self {
        Self {
            base_revision: None,
            ..self.clone()
        }
    }
}

fn rejection(code: RejectionCode, field: &str, issue: &str, message: String) -> Rejection {
    Rejection::new(code, message.clone()).with_field(FieldIssue::new(field, issue, message))
}

/// The template of title input `input`.
fn template_of(state: &ShowState, input: InputId) -> Result<&EngineTitleTemplate, Rejection> {
    let title = state.title(input).ok_or_else(|| {
        rejection(
            RejectionCode::NotFound,
            "input",
            "unknown_title",
            format!("input {input} is not a title"),
        )
        .with_reason(fm_command::RefusalReason::UnknownId {
            kind: fm_command::IdKind::Title,
            id: input.get().get(),
        })
    })?;
    title.template.as_ref().ok_or_else(|| {
        rejection(
            RejectionCode::NotFound,
            "input",
            "no_template",
            format!("title {input} has no loaded template to animate"),
        )
    })
}

fn edit_rejection(error: &AnimationEditError) -> Rejection {
    let message = error.to_string();
    match error {
        AnimationEditError::UnknownElement(_) => rejection(
            RejectionCode::NotFound,
            "element",
            "unknown_element",
            message,
        ),
        AnimationEditError::RevisionExhausted => rejection(
            RejectionCode::ResourceExhausted,
            "base_revision",
            "revision_exhausted",
            message,
        ),
        AnimationEditError::Clip(clip) => {
            let (field, issue) = match clip {
                ClipError::PhaseTooLong { .. } | ClipError::ZeroSpan => ("frames", "invalid"),
                ClipError::TooManyTracks { .. }
                | ClipError::TooManyTrackKeys { .. }
                | ClipError::TooManyKeys { .. } => ("clip", "too_large"),
                ClipError::RetimeCollision { .. } | ClipError::KeyCollision { .. } => {
                    ("frames", "key_collision")
                }
                _ => ("clip", "invalid"),
            };
            rejection(RejectionCode::InvalidCommand, field, issue, message)
        }
    }
}

/// The template `edit` produces from the current state, checked against its
/// base revision. Nothing in `state` changes.
pub(crate) fn edited_template(
    state: &ShowState,
    edit: &EngineTitleAnimationEdit,
) -> Result<TitleTemplate, Rejection> {
    let current = template_of(state, edit.input)?;
    let actual = current.animation().revision;
    if let Some(expected) = edit.base_revision
        && expected != actual
    {
        return Err(rejection(
            RejectionCode::Conflict,
            "base_revision",
            "revision_conflict",
            format!(
                "animation edit is based on revision {expected}, but the animation is at \
                 revision {actual}"
            ),
        ));
    }
    let mut template = (*current.template).clone();
    let result = match &edit.change {
        TitleAnimationChange::Set(clip) => template.edit_animation(|draft| {
            draft.clone_from(clip);
            Ok(())
        }),
        TitleAnimationChange::Preset {
            element,
            phase,
            preset,
            frames,
        } => template.apply_animation_preset(*element, *phase, *preset, *frames),
        TitleAnimationChange::Stretch { phase, frames } => {
            template.stretch_animation(*phase, *frames)
        }
        TitleAnimationChange::Motion {
            element,
            phase,
            preset,
            frames,
        } => {
            let motion = crate::animation_preset::motion_of(state, *preset)?;
            template.apply_animation_motion(*element, *phase, motion, *frames)
        }
    };
    result.map_err(|error| edit_rejection(&error))?;
    Ok(template)
}

/// `edit` with its change made explicit as the exact clip it produces on
/// `state`, so history records and replays it exactly. An edit that would
/// be refused is returned unchanged; execution reports the refusal.
pub(crate) fn normalized(
    state: &ShowState,
    edit: &EngineTitleAnimationEdit,
) -> EngineTitleAnimationEdit {
    match edited_template(state, edit) {
        Ok(template) => EngineTitleAnimationEdit {
            change: TitleAnimationChange::Set(Box::new(template.animation.clip)),
            ..edit.clone()
        },
        Err(_) => edit.clone(),
    }
}

/// The edit that restores the clip `edit`'s template holds now, or `None`
/// when `edit` targets no animated title.
pub(crate) fn inverse(
    state: &ShowState,
    edit: &EngineTitleAnimationEdit,
) -> Option<EngineTitleAnimationEdit> {
    let current = template_of(state, edit.input).ok()?;
    Some(EngineTitleAnimationEdit {
        input: edit.input,
        base_revision: None,
        change: TitleAnimationChange::Set(Box::new(current.animation().clip.clone())),
    })
}

/// Every title input that shares `input`'s template, ascending; empty when
/// `input` is not an animated title.
#[must_use]
pub fn title_template_inputs(state: &ShowState, input: InputId) -> Vec<InputId> {
    let Ok(current) = template_of(state, input) else {
        return Vec::new();
    };
    state
        .titles()
        .iter()
        .filter(|(_, title)| {
            title
                .template
                .as_ref()
                .is_some_and(|template| template.key == current.key)
        })
        .map(|(input, _)| *input)
        .collect()
}

/// Shows or hides one field of a title template in the browser title
/// editor (GX-006, Protocol 2.90). The flag is the template's, so every
/// title input sharing it changes. Not undoable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineTitleExposureEdit {
    pub input: InputId,
    pub field: FieldId,
    pub exposed: bool,
}

/// Applies an exposure edit to `input`'s template and to the field set of
/// every title input sharing it. Setting the flag it already has is
/// accepted and changes nothing.
pub(crate) fn apply_exposure(
    state: &mut ShowState,
    edit: &EngineTitleExposureEdit,
) -> Result<(), Rejection> {
    let current = template_of(state, edit.input)?;
    let mut template = (*current.template).clone();
    let Some(definition) = template
        .fields
        .iter_mut()
        .find(|definition| definition.id == edit.field)
    else {
        return Err(rejection(
            RejectionCode::NotFound,
            "field",
            "unknown_field",
            format!(
                "title {} template has no field {}",
                edit.input,
                edit.field.get()
            ),
        ));
    };
    definition.exposed = edit.exposed;
    let template = Arc::new(template);
    for input in title_template_inputs(state, edit.input) {
        if let Some(title) = state.title_mut(input) {
            title.fields.set_exposed(edit.field, edit.exposed);
            if let Some(shared) = title.template.as_mut() {
                shared.template = Arc::clone(&template);
            }
        }
    }
    Ok(())
}

/// Applies one checked edit: every title input sharing the template takes
/// the new template and its phase lengths.
pub(crate) fn apply(
    state: &mut ShowState,
    edit: &EngineTitleAnimationEdit,
    bound: Option<TitleClipBound>,
) -> Result<(), Rejection> {
    let template = edited_template(state, edit)?;
    if let Some(fits) = bound
        && !fits(&template.animation.clip)
    {
        return Err(rejection(
            RejectionCode::InvalidCommand,
            "clip",
            "too_large",
            "the edited animation is too large to carry".into(),
        ));
    }
    let template = Arc::new(template);
    let inputs = title_template_inputs(state, edit.input);
    for input in inputs {
        if let Some(title) = state.title_mut(input)
            && let Some(shared) = title.template.as_mut()
        {
            shared.template = Arc::clone(&template);
            title.timing = shared.timing();
        }
    }
    Ok(())
}
