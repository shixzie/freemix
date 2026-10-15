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
//!
//! The binding is generic over the clip target: [`TitleAnimationBinding`]
//! edits a title template, [`SceneAnimationBinding`] a scene's layers
//! (GX-011 stage 5). Both have exact previews
//! ([`AnimationBinding::poll_preview`]): a title's as a
//! `title_preview_request`, a scene's as a `scene_preview_request`
//! (Protocol 2.32).

use core::time::Duration;
use std::sync::Arc;

use fm_animation::Clip;
use fm_animation::Phase;
use fm_protocol::{
    CommandPayload, MAX_TITLE_PREVIEW_HEIGHT, MAX_TITLE_PREVIEW_WIDTH, WireSceneLayerId,
    WireSceneLayout, WireTitleElementId, WireTitleElementKind, WireTitleLayout,
};
use fm_types::InputId;

use super::{
    AnimationStatus, AnimationTarget, AuthoritySync, preview_request_id, preview_sample,
    preview_wire_id, save_payload, sync_document,
};
use crate::animation_editor::{
    AnimDocument, PresetSource, PreviewCompletion, PreviewKey, PreviewScheduler, TargetBounds,
    TargetKind, TargetOutline,
};

/// Shortest gap between two exact preview requests: the server refuses
/// more than ten per second per session.
pub const PREVIEW_MIN_INTERVAL: Duration = Duration::from_millis(120);
/// How long an exact preview may stay unanswered before it is given up
/// (previews are lossy; a lost answer must not block the next one).
pub const PREVIEW_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the operator must rest on one preset in the picker before its
/// exact preview is asked for (the fast proxy preview is immediate).
pub const PRESET_HOVER_SETTLE: Duration = Duration::from_millis(250);
/// Preview generations at and above this mark preset-hover previews, so
/// they never match a draft generation (which counts up from zero).
const HOVER_GENERATION_BASE: u64 = u64::MAX / 2;
/// The canvas assumed until the template's design is known.
pub const DEFAULT_CANVAS: (u32, u32) = (1_920, 1_080);

/// A template's or scene's static design as the editor needs it: the
/// canvas size and one outline per target (name, kind, placement, opacity,
/// and fill), in design order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnimationDesign<T> {
    pub canvas: (u32, u32),
    pub elements: Vec<TargetOutline<T>>,
}

/// A title template's design.
pub type TitleDesign = AnimationDesign<WireTitleElementId>;
/// A scene's design (GX-011 stage 5).
pub type SceneDesign = AnimationDesign<WireSceneLayerId>;

impl AnimationDesign<WireSceneLayerId> {
    /// The design a replicated `scene_layout_state` describes: one video
    /// outline per layer at its static on-screen footprint (a quarter turn
    /// swaps the box's sides), named as sent. A disabled layer keeps its
    /// outline at zero opacity. Presets measure the same footprint the
    /// authority's `scene_animation_preset` does.
    #[must_use]
    pub fn from_layout(layout: &WireSceneLayout) -> Self {
        let elements = layout
            .layers
            .iter()
            .map(|layer| {
                let (width, height) = if layer.quarter_turns % 2 == 1 {
                    (layer.height, layer.width)
                } else {
                    (layer.width, layer.height)
                };
                let mut outline = TargetOutline::new(
                    layer.id,
                    layer.name.clone(),
                    TargetKind::Video,
                    TargetBounds::new(layer.x, layer.y, width, height),
                );
                outline.opacity = if layer.enabled { layer.opacity } else { 0 };
                // Crop keys are pixels of the layer's own (unturned) frame.
                outline.frame_size = Some((layer.width, layer.height));
                outline
            })
            .collect();
        Self {
            canvas: (layout.width.max(1), layout.height.max(1)),
            elements,
        }
    }
}

impl AnimationDesign<WireTitleElementId> {
    /// The design a replicated `title_layout_state` describes, or `None` for
    /// a title without a loaded template (a zero canvas). Element kinds map
    /// to outline kinds (clocks and tickers draw text); a hidden element
    /// keeps its outline at zero opacity. Names are kept as sent (possibly
    /// empty; the editor numbers unnamed elements).
    #[must_use]
    pub fn from_layout(layout: &WireTitleLayout) -> Option<Self> {
        if layout.width == 0 || layout.height == 0 {
            return None;
        }
        let elements = layout
            .elements
            .iter()
            .map(|element| {
                let kind = match element.kind {
                    WireTitleElementKind::Text
                    | WireTitleElementKind::Clock
                    | WireTitleElementKind::ProductionClock
                    | WireTitleElementKind::Ticker => TargetKind::Text,
                    WireTitleElementKind::Rectangle => TargetKind::Shape,
                    WireTitleElementKind::Image => TargetKind::Image,
                };
                let mut outline = TargetOutline::new(
                    element.id,
                    element.name.clone(),
                    kind,
                    TargetBounds::new(element.x, element.y, element.width, element.height),
                );
                outline.opacity = if element.visible { element.opacity } else { 0 };
                outline.color = element.fill;
                outline
            })
            .collect();
        Some(Self {
            canvas: (layout.width, layout.height),
            elements,
        })
    }
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

/// A preset the operator rests on in the Simple-mode picker: previewed,
/// not applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresetHover<T = WireTitleElementId> {
    pub target: T,
    pub phase: Phase,
    /// A built-in preset or a saved user preset (Protocol 2.34).
    pub preset: PresetSource,
    /// The preset's length in frames.
    pub frames: u32,
}

/// The draft with a hovered preset applied, for previews only.
#[derive(Clone, Debug)]
struct HoverPreview<T> {
    hover: PresetHover<T>,
    document: AnimDocument<T>,
    key: PreviewKey,
    since: Duration,
    /// The draft generation the preview was built from.
    source: u64,
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
struct SentDraft<T> {
    clip: Arc<Clip<T>>,
    base_revision: u64,
    generation: u64,
}

/// One draft animation bound to its replicated authority.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // each flag is a distinct, independent draft fact
pub struct AnimationBinding<T> {
    input: InputId,
    document: AnimDocument<T>,
    authority: AnimationStatus<T>,
    design: bool,
    sent: Option<SentDraft<T>>,
    conflict: bool,
    /// The draft generation the server refused; a later edit clears it.
    refused: Option<u64>,
    auto_save: bool,
    save_requested: bool,
    previews: PreviewScheduler,
    preview_sent_at: Option<Duration>,
    last_preview_issue: Option<Duration>,
    shown: Option<PreviewKey>,
    hover: Option<HoverPreview<T>>,
    hover_serial: u64,
}

/// One title template's draft animation bound to its replicated authority.
pub type TitleAnimationBinding = AnimationBinding<WireTitleElementId>;
/// One scene's draft layer animation bound to its replicated authority
/// (GX-011 stage 5), previewed exactly through `scene_preview_request`.
pub type SceneAnimationBinding = AnimationBinding<WireSceneLayerId>;

impl<T: AnimationTarget> AnimationBinding<T> {
    /// A clean draft of `input`'s animation as the authority reports it, on
    /// [`DEFAULT_CANVAS`] until [`Self::set_design`]. Auto-save is on.
    #[must_use]
    pub fn new(input: InputId, status: &AnimationStatus<T>) -> Self {
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
            hover: None,
            hover_serial: 0,
        }
    }

    /// The title (or Scene) input whose animation this draft edits.
    #[must_use]
    pub const fn input(&self) -> InputId {
        self.input
    }

    #[must_use]
    pub const fn document(&self) -> &AnimDocument<T> {
        &self.document
    }

    /// The draft, for the editor widgets. Saves leave through
    /// [`Self::poll_save`].
    pub const fn document_mut(&mut self) -> &mut AnimDocument<T> {
        &mut self.document
    }

    /// The authority's animation as last seen by [`Self::sync`].
    #[must_use]
    pub const fn authority(&self) -> &AnimationStatus<T> {
        &self.authority
    }

    /// Gives the editor the canvas and target outlines, so presets and the
    /// wireframe proxy know where each target sits.
    pub fn set_design(&mut self, design: &AnimationDesign<T>) {
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
    pub fn sync(&mut self, status: &AnimationStatus<T>) -> AuthoritySync {
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
    /// While a preset is hovered, the wish is for the hovered draft, once
    /// the hover has rested for [`PRESET_HOVER_SETTLE`].
    pub fn want_preview(&mut self, now: Duration) {
        match &self.hover {
            Some(hover) if now.saturating_sub(hover.since) >= PRESET_HOVER_SETTLE => {
                self.previews.request(hover.key, now);
            }
            Some(_) => {}
            None => self.previews.request(self.document.preview_key(), now),
        }
    }

    // ----- preset hover previews ---------------------------------------------

    /// Previews `hover` (or stops previewing with `None`) at time `now`:
    /// the draft with the preset applied becomes
    /// [`Self::preset_hover_document`] for a fast proxy, and
    /// [`Self::want_preview`] asks for its exact preview at the middle of
    /// the phase once the hover rests. Nothing is applied or saved. A
    /// preset that cannot apply (no outline for the element, a limit, a
    /// drag) previews nothing. Repeating the current hover over an
    /// unchanged draft is free.
    pub fn set_preset_hover(&mut self, hover: Option<PresetHover<T>>, now: Duration) {
        let generation = self.document.generation();
        if self
            .hover
            .as_ref()
            .map(|preview| (&preview.hover, preview.source))
            == hover.as_ref().map(|hover| (hover, generation))
        {
            return;
        }
        self.hover = None;
        let Some(hover) = hover else {
            return;
        };
        let source = &self.document;
        let (width, height) = source.canvas();
        let mut document =
            AnimDocument::new(source.clip().clone(), source.base_revision(), width, height);
        document.set_targets(source.targets().iter().cloned());
        if document
            .apply_preset_source(
                hover.target,
                hover.phase,
                &hover.preset,
                hover.frames,
                T::PROPERTIES,
            )
            .is_err()
        {
            return;
        }
        document.clear_selection();
        let frame = document.phase_frames() / 2;
        document.set_playhead(frame);
        self.hover_serial = self.hover_serial.wrapping_add(1);
        let key = PreviewKey {
            generation: HOVER_GENERATION_BASE
                .saturating_add(self.hover_serial % HOVER_GENERATION_BASE),
            phase: hover.phase,
            frame,
        };
        self.hover = Some(HoverPreview {
            hover,
            document,
            key,
            since: now,
            source: generation,
        });
    }

    /// The preset being previewed, if any.
    #[must_use]
    pub fn preset_hover(&self) -> Option<PresetHover<T>> {
        self.hover.as_ref().map(|preview| preview.hover.clone())
    }

    /// The draft with the hovered preset applied (for the fast proxy).
    #[must_use]
    pub fn preset_hover_document(&self) -> Option<&AnimDocument<T>> {
        self.hover.as_ref().map(|preview| &preview.document)
    }

    /// The hovered draft, to move its playhead for the fast proxy.
    pub fn preset_hover_document_mut(&mut self) -> Option<&mut AnimDocument<T>> {
        self.hover.as_mut().map(|preview| &mut preview.document)
    }

    /// Whether the last received exact preview shows the hovered preset.
    #[must_use]
    pub fn hover_preview_current(&self) -> bool {
        self.hover
            .as_ref()
            .is_some_and(|preview| self.shown == Some(preview.key))
    }

    /// Whether the last received exact preview showed a hovered preset
    /// rather than the draft.
    #[must_use]
    pub fn shown_preview_is_hover(&self) -> bool {
        self.shown
            .is_some_and(|key| key.generation >= HOVER_GENERATION_BASE)
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
    pub fn reset_previews(&mut self) {
        self.previews.reset();
        self.preview_sent_at = None;
        self.last_preview_issue = None;
        self.shown = None;
        self.hover = None;
    }

    /// The exact preview request to send now (`title_preview_request` or
    /// `scene_preview_request`), if the debounce elapsed, nothing is in
    /// flight, the last request is [`PREVIEW_MIN_INTERVAL`] old, and
    /// `can_request` holds. An answer missing for [`PREVIEW_TIMEOUT`] is
    /// given up first.
    pub fn poll_preview(&mut self, now: Duration, can_request: bool) -> Option<T::PreviewRequest> {
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
        let hovered = self
            .hover
            .as_ref()
            .filter(|preview| preview.key == request.key)
            .map(|preview| preview.document.clip());
        // A hovered preset, or a draft that differs from the saved
        // animation (a drag changes it before it is marked dirty), travels
        // along.
        let draft = hovered.unwrap_or_else(|| self.document.clip());
        Some(T::preview(
            self.input,
            preview_wire_id(request.id),
            preview_sample(request),
            (width, height),
            (draft != &*self.authority.clip).then(|| draft.clone()),
        ))
    }
}

#[cfg(test)]
mod tests;
