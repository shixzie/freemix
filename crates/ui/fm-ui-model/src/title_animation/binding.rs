//! Binds one title template's [`TitleAnimDocument`] to its replicated
//! animation and paces its saves and exact previews (GX-011, Studio).
//!
//! The authority owns the animation. [`TitleAnimationBinding::sync`] follows
//! every authority report while the draft is clean, recognises this editor's
//! own save coming back (even when the operator kept editing meanwhile),
//! and flags a [`AnimationSaveStatus::Conflict`] when another editor changed
//! the animation under a dirty draft. The operator then either reloads the
//! authority's animation ([`TitleAnimationBinding::reload_theirs`]) or
//! re-bases the draft onto it and saves it over theirs
//! ([`TitleAnimationBinding::overwrite`]).
//!
//! [`TitleAnimationBinding::poll_save`] sends the whole draft as a
//! revision-checked `title_animation_set`: with auto-save on, as soon as the
//! draft changed and no drag is in progress (a drag saves on release), and
//! otherwise when the operator asks. At most one save is in flight.
//!
//! [`TitleAnimationBinding::want_preview`] and
//! [`TitleAnimationBinding::poll_preview`] drive the document's latest-wins
//! [`PreviewScheduler`]: requests are debounced, at most one is in flight,
//! issues are at least [`PREVIEW_MIN_INTERVAL`] apart (under the server's
//! ten per second), and an unanswered request is given up after
//! [`PREVIEW_TIMEOUT`] so a lost answer never stalls previews.

use core::time::Duration;
use std::sync::Arc;

use fm_protocol::{
    CommandPayload, MAX_TITLE_PREVIEW_HEIGHT, MAX_TITLE_PREVIEW_WIDTH, TitlePreviewRequest,
    WireTitleClip, WireTitleElementId,
};
use fm_types::InputId;

use super::{
    AuthoritySync, TitleAnimDocument, TitleAnimationStatus, preview_request, preview_request_id,
    save_payload, sync_document,
};
use crate::animation_editor::{
    AnimDocument, PreviewCompletion, PreviewKey, PreviewScheduler, TargetOutline,
};

/// Shortest gap between two exact preview requests: the server refuses
/// more than ten per second per session.
pub const PREVIEW_MIN_INTERVAL: Duration = Duration::from_millis(120);
/// How long an exact preview may stay unanswered before it is given up
/// (previews are lossy; a lost answer must not block the next one).
pub const PREVIEW_TIMEOUT: Duration = Duration::from_secs(2);
/// The canvas assumed until the template's design is known.
pub const DEFAULT_CANVAS: (u32, u32) = (1_920, 1_080);

/// A title template's static design as the editor needs it: the canvas
/// size and one outline per element (name, kind, placement, opacity, and
/// fill), in design order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleDesign {
    pub canvas: (u32, u32),
    pub elements: Vec<TargetOutline<WireTitleElementId>>,
}

/// The exact preview size for `canvas`: the largest size of the same shape
/// within the protocol's 640x360 (each side at least one pixel).
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped into 1..=640 and 1..=360.
pub fn preview_size(canvas: (u32, u32)) -> (u16, u16) {
    let (width, height) = (f64::from(canvas.0.max(1)), f64::from(canvas.1.max(1)));
    let max_width = f64::from(MAX_TITLE_PREVIEW_WIDTH);
    let max_height = f64::from(MAX_TITLE_PREVIEW_HEIGHT);
    let scale = (max_width / width).min(max_height / height);
    let fit = |side: f64, max: f64| (side * scale).round().clamp(1.0, max) as u16;
    (fit(width, max_width), fit(height, max_height))
}

/// How the draft relates to the template's saved animation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AnimationSaveStatus {
    /// The draft is the saved animation.
    Saved,
    /// The draft has changes not sent yet (a drag in progress, auto-save
    /// off, or edits held back by a padlock or permission).
    Unsaved,
    /// The draft was sent; the server has not reported it yet.
    Saving,
    /// Another editor changed the animation under this draft: reload theirs
    /// or overwrite it.
    Conflict,
    /// The server refused the last save (for example a padlock); edit again
    /// or save to retry.
    Refused,
}

/// Why the server refused a save.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AnimationRefusal {
    /// The draft was based on a stale animation revision.
    Conflict,
    /// Any other refusal (padlock, permission, invalid clip).
    Other,
}

/// A save on its way to the server.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SentDraft {
    clip: Arc<WireTitleClip>,
    base_revision: u64,
    generation: u64,
}

/// One title template's draft animation bound to its replicated authority.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // each flag is a distinct, independent draft fact
pub struct TitleAnimationBinding {
    input: InputId,
    document: TitleAnimDocument,
    authority: TitleAnimationStatus,
    design: bool,
    sent: Option<SentDraft>,
    conflict: bool,
    /// The draft generation the server refused; a later edit clears it.
    refused: Option<u64>,
    auto_save: bool,
    save_requested: bool,
    previews: PreviewScheduler,
    preview_sent_at: Option<Duration>,
    last_preview_issue: Option<Duration>,
    shown: Option<PreviewKey>,
}

impl TitleAnimationBinding {
    /// A clean draft of `input`'s template animation as the authority
    /// reports it, on [`DEFAULT_CANVAS`] until [`Self::set_design`]. Auto-save
    /// is on.
    #[must_use]
    pub fn new(input: InputId, status: &TitleAnimationStatus) -> Self {
        let document = AnimDocument::new(
            (*status.clip).clone(),
            status.revision,
            DEFAULT_CANVAS.0,
            DEFAULT_CANVAS.1,
        );
        Self {
            input,
            document,
            authority: status.clone(),
            design: false,
            sent: None,
            conflict: false,
            refused: None,
            auto_save: true,
            save_requested: false,
            previews: PreviewScheduler::default(),
            preview_sent_at: None,
            last_preview_issue: None,
            shown: None,
        }
    }

    /// The title input whose template this draft edits.
    #[must_use]
    pub const fn input(&self) -> InputId {
        self.input
    }

    #[must_use]
    pub const fn document(&self) -> &TitleAnimDocument {
        &self.document
    }

    /// The draft, for the editor widgets. Saves leave through
    /// [`Self::poll_save`].
    pub const fn document_mut(&mut self) -> &mut TitleAnimDocument {
        &mut self.document
    }

    /// The authority's animation as last seen by [`Self::sync`].
    #[must_use]
    pub const fn authority(&self) -> &TitleAnimationStatus {
        &self.authority
    }

    /// Gives the editor the template's canvas and element outlines, so
    /// presets and the wireframe proxy know where each element sits.
    pub fn set_design(&mut self, design: &TitleDesign) {
        self.document.set_canvas(design.canvas.0, design.canvas.1);
        self.document.set_targets(design.elements.iter().cloned());
        self.design = true;
    }

    /// Whether [`Self::set_design`] gave the editor the element outlines.
    #[must_use]
    pub const fn has_design(&self) -> bool {
        self.design
    }

    /// Brings the draft in step with the authority's `status`: a clean
    /// draft follows it, this editor's own save coming back is recognised
    /// (the draft stays dirty if the operator kept editing), and another
    /// editor's change under a dirty draft is a conflict. Nothing changes
    /// during a drag; the next call after it catches up.
    pub fn sync(&mut self, status: &TitleAnimationStatus) -> AuthoritySync {
        self.authority = status.clone();
        if let Some(sent) = &self.sent
            && *status.clip == *sent.clip
            && status.revision > sent.base_revision
        {
            let unchanged = self.document.generation() == sent.generation;
            self.sent = None;
            self.conflict = false;
            self.refused = None;
            if unchanged && self.document.drag().is_none() {
                self.document.mark_saved(status.revision);
            } else {
                // Edits made while the save travelled build on it.
                self.document.base_revision = status.revision;
            }
            return AuthoritySync::Saved;
        }
        if self.document.drag().is_some() {
            return AuthoritySync::Unchanged;
        }
        let outcome = sync_document(&mut self.document, status);
        match outcome {
            AuthoritySync::Reset => {
                self.refused = None;
                self.save_requested = false;
                self.conflict = false;
            }
            AuthoritySync::Saved => {
                self.sent = None;
                self.refused = None;
                self.conflict = false;
            }
            AuthoritySync::Conflict => self.conflict = true,
            AuthoritySync::Unchanged => {}
        }
        outcome
    }

    /// Discards the draft (and its local history) for the authority's
    /// animation.
    pub fn reload_theirs(&mut self) {
        self.document
            .reset_from_authority((*self.authority.clip).clone(), self.authority.revision);
        self.sent = None;
        self.conflict = false;
        self.refused = None;
        self.save_requested = false;
    }

    /// Keeps the draft and saves it over the authority's animation: the
    /// draft is re-based onto the authority's revision and sent by the next
    /// [`Self::poll_save`].
    pub fn overwrite(&mut self) {
        self.document.base_revision = self.authority.revision;
        self.document.dirty = true;
        self.conflict = false;
        self.refused = None;
        self.save_requested = true;
    }

    /// Records that the server refused the save in flight.
    pub fn refused(&mut self, refusal: AnimationRefusal) {
        self.sent = None;
        self.save_requested = false;
        match refusal {
            AnimationRefusal::Conflict => self.conflict = true,
            AnimationRefusal::Other => self.refused = Some(self.document.generation()),
        }
    }

    /// Forgets a save whose answer will not arrive (never sent, or the
    /// connection dropped); the draft stays dirty and is sent again.
    pub fn abandon_in_flight(&mut self) {
        self.sent = None;
    }

    #[must_use]
    pub const fn auto_save(&self) -> bool {
        self.auto_save
    }

    /// Turns auto-save on or off. Off, the draft is sent only by
    /// [`Self::request_save`].
    pub const fn set_auto_save(&mut self, on: bool) {
        self.auto_save = on;
    }

    /// Asks the next [`Self::poll_save`] to send the draft (also retrying a
    /// refused one).
    pub const fn request_save(&mut self) {
        self.save_requested = true;
        self.refused = None;
    }

    /// Whether a save is on its way.
    #[must_use]
    pub const fn saving(&self) -> bool {
        self.sent.is_some()
    }

    /// The `title_animation_set` to send now, if any: never during a drag,
    /// a conflict, or with a save in flight, nor when `can_send` is false
    /// (the draft then stays unsaved). With auto-save off only after
    /// [`Self::request_save`]; a refused draft only after an edit or a
    /// request. A draft undone back to the saved animation is simply marked
    /// saved.
    pub fn poll_save(&mut self, can_send: bool) -> Option<CommandPayload> {
        if !can_send
            || self.sent.is_some()
            || self.conflict
            || self.document.drag().is_some()
            || !self.document.is_dirty()
        {
            return None;
        }
        if self.document.base_revision() == self.authority.revision
            && self.document.clip() == &*self.authority.clip
        {
            self.document.mark_saved(self.authority.revision);
            self.save_requested = false;
            return None;
        }
        let refused = self.refused == Some(self.document.generation());
        if !self.save_requested && (!self.auto_save || refused) {
            return None;
        }
        let payload = save_payload(&self.document, self.input)?;
        self.sent = Some(SentDraft {
            clip: Arc::new(self.document.clip().clone()),
            base_revision: self.document.base_revision(),
            generation: self.document.generation(),
        });
        self.save_requested = false;
        self.refused = None;
        Some(payload)
    }

    /// How the draft relates to the saved animation now.
    #[must_use]
    pub fn status(&self) -> AnimationSaveStatus {
        if self.conflict {
            AnimationSaveStatus::Conflict
        } else if self.sent.is_some() {
            AnimationSaveStatus::Saving
        } else if self.refused == Some(self.document.generation()) {
            AnimationSaveStatus::Refused
        } else if self.document.is_dirty()
            || (self.document.drag().is_some() && self.document.clip() != &*self.authority.clip)
        {
            // A drag changes the draft before it is marked dirty.
            AnimationSaveStatus::Unsaved
        } else {
            AnimationSaveStatus::Saved
        }
    }

    // ----- exact previews ----------------------------------------------------

    /// Wants an exact preview of the draft at the playhead (call each frame
    /// the canvas shows; repeated wishes for the same frame are free).
    pub fn want_preview(&mut self, now: Duration) {
        self.previews.request(self.document.preview_key(), now);
    }

    /// The `title_preview_request` to send now, if the debounce elapsed,
    /// nothing is in flight, the last request is [`PREVIEW_MIN_INTERVAL`]
    /// old, and `can_request` holds. An answer missing for
    /// [`PREVIEW_TIMEOUT`] is given up first.
    pub fn poll_preview(
        &mut self,
        now: Duration,
        can_request: bool,
    ) -> Option<TitlePreviewRequest> {
        if let (Some(request), Some(sent)) = (self.previews.in_flight(), self.preview_sent_at)
            && now.saturating_sub(sent) >= PREVIEW_TIMEOUT
        {
            self.previews.fail(request.id);
            self.preview_sent_at = None;
        }
        if !can_request
            || self
                .last_preview_issue
                .is_some_and(|last| now.saturating_sub(last) < PREVIEW_MIN_INTERVAL)
        {
            return None;
        }
        let request = self.previews.poll(now)?;
        self.last_preview_issue = Some(now);
        self.preview_sent_at = Some(now);
        let (width, height) = preview_size(self.document.canvas());
        let mut wire = preview_request(&self.document, self.input, request, width, height);
        // A drag in progress changes the draft before it is marked dirty:
        // any draft that differs from the saved animation travels along.
        wire.clip = (self.document.clip() != &*self.authority.clip)
            .then(|| Box::new(self.document.clip().clone()));
        Some(wire)
    }

    /// When the next preview could go out or the one in flight times out,
    /// for scheduling a repaint.
    #[must_use]
    pub fn next_preview_deadline(&self) -> Option<Duration> {
        let paced = |deadline: Duration| {
            self.last_preview_issue
                .map_or(deadline, |last| deadline.max(last + PREVIEW_MIN_INTERVAL))
        };
        let issue = self.previews.next_deadline().map(paced);
        let timeout = self
            .previews
            .in_flight()
            .and(self.preview_sent_at)
            .map(|sent| sent + PREVIEW_TIMEOUT);
        match (issue, timeout) {
            (Some(issue), Some(timeout)) => Some(issue.min(timeout)),
            (issue, timeout) => issue.or(timeout),
        }
    }

    /// Records the answer to the preview with wire id `request_id`. Returns
    /// how it relates to what the editor wants now, or `None` for an answer
    /// this binding is not waiting for (ignore it).
    pub fn receive_preview(&mut self, request_id: &str) -> Option<PreviewCompletion> {
        let id = preview_request_id(request_id)?;
        let key = self
            .previews
            .in_flight()
            .filter(|request| request.id == id)?
            .key;
        let completion = self.previews.complete(id);
        self.preview_sent_at = None;
        self.shown = Some(key);
        Some(completion)
    }

    /// Records that the preview with wire id `request_id` was refused or
    /// lost; returns whether it was the one in flight.
    pub fn preview_failed(&mut self, request_id: &str) -> bool {
        let Some(id) = preview_request_id(request_id) else {
            return false;
        };
        let failed = self.previews.fail(id);
        if failed {
            self.preview_sent_at = None;
        }
        failed
    }

    /// Whether `request_id` is the exact preview in flight.
    #[must_use]
    pub fn awaits_preview(&self, request_id: &str) -> bool {
        preview_request_id(request_id).is_some_and(|id| {
            self.previews
                .in_flight()
                .is_some_and(|request| request.id == id)
        })
    }

    /// What the last received exact preview shows.
    #[must_use]
    pub const fn shown_preview(&self) -> Option<PreviewKey> {
        self.shown
    }

    /// Whether the last received exact preview shows exactly the draft at
    /// the playhead.
    #[must_use]
    pub fn exact_preview_current(&self) -> bool {
        self.shown == Some(self.document.preview_key())
    }

    /// Forgets every preview request and answer (for example after a
    /// reconnect or a lost image).
    pub const fn reset_previews(&mut self) {
        self.previews.reset();
        self.preview_sent_at = None;
        self.last_preview_issue = None;
        self.shown = None;
    }
}

#[cfg(test)]
mod tests;
