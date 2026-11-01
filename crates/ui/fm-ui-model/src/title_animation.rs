//! Replicated title animations and the adapter between the authority and
//! the headless [`AnimDocument`] editor (GX-011 stage 4a, Protocol 2.29).
//!
//! The authority reports each title's template animation in a
//! `title_animation_state` after a snapshot and in `title_animation_changed`
//! after an edit, undo, or redo; [`crate::TitleStatus::animation`] holds the
//! latest. An editor keeps its [`AnimDocument`] in step with
//! [`sync_document`], saves its draft with [`save_payload`] (a whole-clip,
//! revision-checked `title_animation_set`), and turns each
//! [`ExactPreviewRequest`] of its [`crate::animation_editor::PreviewScheduler`]
//! into a `title_preview_request` (a scene's into a `scene_preview_request`,
//! Protocol 2.32) with [`preview_request`], matching the answer back with
//! [`preview_request_id`].
//!
//! [`TitleAnimationBinding`] packages all of that for a Studio window: it
//! owns the document, recognises its own saves, flags conflicts, paces
//! auto-saves and exact previews (also of a preset the operator is only
//! hovering), and takes the template's [`TitleDesign`] (canvas and element
//! outlines), built from the replicated `title_layout_state` (Protocol 2.30)
//! with [`TitleDesign::from_layout`].
//!
//! Scene layer animations (GX-011 stage 5, Protocol 2.31) reuse all of it:
//! the binding, document, and status are generic over the clip target
//! ([`AnimationTarget`]), so a scene editor is an
//! [`AnimationBinding`]`<`[`WireSceneLayerId`]`>` ([`SceneAnimationBinding`])
//! saving `scene_animation_set` and designed from the replicated
//! `scene_layout_state` ([`SceneDesign::from_layout`]), and previewed
//! exactly through `scene_preview_request` (Protocol 2.32) like a title.

use std::sync::Arc;

use fm_animation::{Clip, Phase, Property};
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandPayload, ScenePreviewRequest, TitlePreviewRequest,
    TitlePreviewSample, WireInputId, WireSceneLayerId, WireTitleElementId,
};
use fm_types::InputId;

use crate::animation_editor::{AnimDocument, ExactPreviewRequest, PreviewRequestId};

mod binding;

pub use binding::{
    AnimationBinding, AnimationDesign, AnimationRefusal, AnimationSaveStatus, DEFAULT_CANVAS,
    PRESET_HOVER_SETTLE, PREVIEW_MIN_INTERVAL, PREVIEW_TIMEOUT, PresetHover, SceneAnimationBinding,
    SceneDesign, TitleAnimationBinding, TitleDesign, preview_size,
};

/// What an animation editor can target: a title element or a scene layer.
/// The target decides the edit command a save sends, the exact preview
/// request it asks for, and which properties may be animated.
pub trait AnimationTarget: Copy + Ord + core::fmt::Debug {
    /// The properties a target of this kind animates, in display order.
    const PROPERTIES: &'static [Property];

    /// The exact preview request of this target's owner:
    /// `title_preview_request` or `scene_preview_request` (Protocol 2.32).
    type PreviewRequest;

    /// The revision-checked whole-clip save of `clip` through `input`.
    fn set_payload(input: InputId, base_revision: u64, clip: Clip<Self>) -> CommandPayload;

    /// The exact preview of `input` at `sample` and `(width, height)`, with
    /// `clip` as an unsaved draft when given.
    fn preview(
        input: InputId,
        request_id: String,
        sample: TitlePreviewSample,
        size: (u16, u16),
        clip: Option<Clip<Self>>,
    ) -> Self::PreviewRequest;
}

impl AnimationTarget for WireTitleElementId {
    const PROPERTIES: &'static [Property] = &TITLE_ANIMATION_PROPERTIES;

    type PreviewRequest = TitlePreviewRequest;

    fn set_payload(input: InputId, base_revision: u64, clip: Clip<Self>) -> CommandPayload {
        CommandPayload::SetTitleAnimation {
            input: WireInputId::from_domain(input),
            base_revision,
            clip: Box::new(clip),
        }
    }

    fn preview(
        input: InputId,
        request_id: String,
        sample: TitlePreviewSample,
        (width, height): (u16, u16),
        clip: Option<Clip<Self>>,
    ) -> TitlePreviewRequest {
        TitlePreviewRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id,
            input: WireInputId::from_domain(input),
            sample,
            width,
            height,
            clip: clip.map(Box::new),
            fields: Vec::new(),
        }
    }
}

/// Every property a title element animates: the whole target property set
/// except the garbage matte coordinates, which only scene layers carry.
pub const TITLE_ANIMATION_PROPERTIES: [Property; 11] = [
    Property::PositionX,
    Property::PositionY,
    Property::ScaleX,
    Property::ScaleY,
    Property::Rotation,
    Property::Opacity,
    Property::CropLeft,
    Property::CropTop,
    Property::CropRight,
    Property::CropBottom,
    Property::Color,
];

/// Every property a scene layer animates: all but [`Property::Color`]
/// (layers have no fill colour), including its garbage matte's points.
pub const SCENE_LAYER_PROPERTIES: [Property; 10 + fm_types::MAX_GARBAGE_MATTE_POINTS * 2] = [
    Property::PositionX,
    Property::PositionY,
    Property::ScaleX,
    Property::ScaleY,
    Property::Rotation,
    Property::Opacity,
    Property::CropLeft,
    Property::CropTop,
    Property::CropRight,
    Property::CropBottom,
    Property::MattePoint1X,
    Property::MattePoint1Y,
    Property::MattePoint2X,
    Property::MattePoint2Y,
    Property::MattePoint3X,
    Property::MattePoint3Y,
    Property::MattePoint4X,
    Property::MattePoint4Y,
    Property::MattePoint5X,
    Property::MattePoint5Y,
    Property::MattePoint6X,
    Property::MattePoint6Y,
    Property::MattePoint7X,
    Property::MattePoint7Y,
    Property::MattePoint8X,
    Property::MattePoint8Y,
];

impl AnimationTarget for WireSceneLayerId {
    const PROPERTIES: &'static [Property] = &SCENE_LAYER_PROPERTIES;

    type PreviewRequest = ScenePreviewRequest;

    fn set_payload(input: InputId, base_revision: u64, clip: Clip<Self>) -> CommandPayload {
        CommandPayload::SetSceneAnimation {
            input: WireInputId::from_domain(input),
            base_revision,
            clip: Box::new(clip),
        }
    }

    fn preview(
        input: InputId,
        request_id: String,
        sample: TitlePreviewSample,
        (width, height): (u16, u16),
        clip: Option<Clip<Self>>,
    ) -> ScenePreviewRequest {
        ScenePreviewRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id,
            input: WireInputId::from_domain(input),
            sample,
            width,
            height,
            clip: clip.map(Box::new),
        }
    }
}

/// The document type an editor of one title template uses.
pub type TitleAnimDocument = AnimDocument<WireTitleElementId>;
/// The document type an editor of one scene uses.
pub type SceneAnimDocument = AnimDocument<WireSceneLayerId>;

/// One animation as the authority last reported it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnimationStatus<T> {
    /// The `base_revision` the next edit must name.
    pub revision: u64,
    pub clip: Arc<Clip<T>>,
}

/// One title template's animation as the authority last reported it.
pub type TitleAnimationStatus = AnimationStatus<WireTitleElementId>;
/// One scene's layer animation as the authority last reported it
/// (Protocol 2.31).
pub type SceneAnimationStatus = AnimationStatus<WireSceneLayerId>;

/// What [`sync_document`] did with an authority report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthoritySync {
    /// The document is already based on this revision.
    Unchanged,
    /// The clean document took the authority's clip and revision.
    Reset,
    /// The authority holds exactly the dirty draft (this editor's save
    /// landed): the draft is now clean at the new revision, history kept.
    Saved,
    /// The authority moved to a different clip while the draft had unsaved
    /// edits. The draft is untouched and still based on its old revision,
    /// so saving it would be refused as a conflict; the caller decides
    /// whether to discard it ([`AnimDocument::reset_from_authority`]).
    Conflict,
}

/// Brings `document` in step with the authority's `status`.
pub fn sync_document<T: Copy + Ord>(
    document: &mut AnimDocument<T>,
    status: &AnimationStatus<T>,
) -> AuthoritySync {
    if document.base_revision() == status.revision {
        return AuthoritySync::Unchanged;
    }
    if !document.is_dirty() {
        document.reset_from_authority((*status.clip).clone(), status.revision);
        return AuthoritySync::Reset;
    }
    if document.clip() == &*status.clip {
        document.mark_saved(status.revision);
        return AuthoritySync::Saved;
    }
    AuthoritySync::Conflict
}

/// The `title_animation_set` (or, for a scene, `scene_animation_set`) that
/// saves `document`'s draft through `input`, or `None` when there is
/// nothing to save.
#[must_use]
pub fn save_payload<T: AnimationTarget>(
    document: &AnimDocument<T>,
    input: InputId,
) -> Option<CommandPayload> {
    document
        .is_dirty()
        .then(|| T::set_payload(input, document.base_revision(), document.clip().clone()))
}

/// The wire `request_id` of exact preview `id`.
#[must_use]
pub fn preview_wire_id(id: PreviewRequestId) -> String {
    format!("anim-preview-{}", id.0)
}

/// The exact preview a `title_preview_frame`, `scene_preview_frame`, or
/// error with wire `request_id` answers, if it is one of [`preview_wire_id`]'s.
#[must_use]
pub fn preview_request_id(request_id: &str) -> Option<PreviewRequestId> {
    let digits = request_id.strip_prefix("anim-preview-")?;
    if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0')) {
        return None;
    }
    digits.parse().ok().map(PreviewRequestId)
}

/// The pose an exact preview of `request` samples: its phase and frame.
#[must_use]
pub const fn preview_sample(request: ExactPreviewRequest) -> TitlePreviewSample {
    let frame = request.key.frame;
    match request.key.phase {
        Phase::In => TitlePreviewSample::In { frame },
        Phase::Out => TitlePreviewSample::Out { frame },
    }
}

/// The exact preview request for `request` of `document` editing `input`
/// (a `title_preview_request` for a template, a `scene_preview_request` for
/// a scene), at `width` x `height` (at most the protocol's 640x360). A dirty
/// draft travels with the request so the authority renders exactly what the
/// editor shows; a clean one renders the saved animation.
#[must_use]
pub fn preview_request<T: AnimationTarget>(
    document: &AnimDocument<T>,
    input: InputId,
    request: ExactPreviewRequest,
    width: u16,
    height: u16,
) -> T::PreviewRequest {
    T::preview(
        input,
        preview_wire_id(request.id),
        preview_sample(request),
        (width, height),
        document.is_dirty().then(|| document.clip().clone()),
    )
}
