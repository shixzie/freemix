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
//! into a `title_preview_request` with [`preview_request`], matching the
//! answer back with [`preview_request_id`].

use std::sync::Arc;

use fm_animation::Phase;
use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, CommandPayload, TitlePreviewRequest, TitlePreviewSample, WireInputId,
    WireTitleClip, WireTitleElementId,
};
use fm_types::InputId;

use crate::animation_editor::{AnimDocument, ExactPreviewRequest, PreviewRequestId};

/// The document type an editor of one title template uses.
pub type TitleAnimDocument = AnimDocument<WireTitleElementId>;

/// One title template's animation as the authority last reported it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleAnimationStatus {
    /// The `base_revision` the next edit must name.
    pub revision: u64,
    pub clip: Arc<WireTitleClip>,
}

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
pub fn sync_document(
    document: &mut TitleAnimDocument,
    status: &TitleAnimationStatus,
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

/// The `title_animation_set` that saves `document`'s draft to `input`'s
/// template, or `None` when there is nothing to save.
#[must_use]
pub fn save_payload(document: &TitleAnimDocument, input: InputId) -> Option<CommandPayload> {
    document
        .is_dirty()
        .then(|| CommandPayload::SetTitleAnimation {
            input: WireInputId::from_domain(input),
            base_revision: document.base_revision(),
            clip: Box::new(document.clip().clone()),
        })
}

/// The wire `request_id` of exact preview `id`.
#[must_use]
pub fn preview_wire_id(id: PreviewRequestId) -> String {
    format!("anim-preview-{}", id.0)
}

/// The exact preview a `title_preview_frame` or error with wire
/// `request_id` answers, if it is one of [`preview_wire_id`]'s.
#[must_use]
pub fn preview_request_id(request_id: &str) -> Option<PreviewRequestId> {
    let digits = request_id.strip_prefix("anim-preview-")?;
    if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0')) {
        return None;
    }
    digits.parse().ok().map(PreviewRequestId)
}

/// The `title_preview_request` for `request` of `document` editing
/// `input`'s template, at `width` x `height` (at most the protocol's
/// 640x360). A dirty draft travels with the request so the authority
/// renders exactly what the editor shows; a clean one renders the saved
/// animation.
#[must_use]
pub fn preview_request(
    document: &TitleAnimDocument,
    input: InputId,
    request: ExactPreviewRequest,
    width: u16,
    height: u16,
) -> TitlePreviewRequest {
    let frame = request.key.frame;
    TitlePreviewRequest {
        protocol: CURRENT_PROTOCOL_VERSION,
        request_id: preview_wire_id(request.id),
        input: WireInputId::from_domain(input),
        sample: match request.key.phase {
            Phase::In => TitlePreviewSample::In { frame },
            Phase::Out => TitlePreviewSample::Out { frame },
        },
        width,
        height,
        clip: document
            .is_dirty()
            .then(|| Box::new(document.clip().clone())),
    }
}
