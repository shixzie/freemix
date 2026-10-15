//! The keyframe animation editor's `egui` widgets: a compact "mini After
//! Effects" over the headless [`AnimDocument`] core.
//!
//! [`AnimationTimelineWidget`] draws, top to bottom:
//!
//! * a header with the phase tabs (`IN`, the non-editable `HOLD`, `OUT`),
//!   the phase length, and zoom-to-fit;
//! * the transport bar ([`transport`]): play/pause, loop, frame steps, the
//!   timecode readout, key at playhead, delete, copy/paste, undo/redo, and
//!   the Simple/Advanced toggle, each with hover text naming its shortcut and
//!   a reason when disabled;
//! * the time ruler and rows ([`tracks`]): in Simple mode one bar per
//!   element with a duration handle (dragging it retimes) and a preset
//!   picker; in Advanced mode one row per property with keys, box select,
//!   snapping guide line, and retime handles on the selection;
//! * in Advanced mode, an inspector ([`inspector`]) with the easing picker
//!   for the selected keys, the active row's value at the playhead, an
//!   "animate property" menu, and a value graph;
//! * an inline notice for refused edits, worded from
//!   [`AnimEditorError::msg_key`] ([`text`]).
//!
//! Every string comes from the shared Studio catalog
//! ([`fm_ui_model::i18n::Msg`]). The widget words itself in the locale the
//! Studio shell set for the frame, or in the one pinned with
//! [`AnimationTimelineWidget::locale`].
//!
//! [`overlay`] draws the proxy quads and onion skins over a preview image.
//!
//! The widget edits the document directly (every completed action is one
//! undo step in the core) and reports what happened in [`TimelineOutput`];
//! it never talks to a server.

use core::time::Duration;

use egui::{CornerRadius, DragValue, Frame, Margin, Rect, Response, RichText, Stroke, Ui, Vec2};
use fm_animation::{MAX_PHASE_FRAMES, Phase, Property, TrackKey};
use fm_ui_model::animation_editor::{
    AnimDocument, AnimEditorError, EditorMode, MAX_FRAMES_PER_PIXEL, MIN_FRAMES_PER_PIXEL,
};
use fm_ui_model::i18n::{Arg, Locale, Msg};

use crate::gating::Hint;
use crate::l10n;
use crate::theme::{
    ACCENT_FILL, AMBER, ERROR, accent_color, border_color, border_strong_color, legible,
    line_width, muted_color, panel_color, section_label, text_color, text_dim_color, widget_color,
};

pub mod inspector;
pub mod overlay;
pub mod text;
pub mod tracks;
pub mod transport;

#[cfg(test)]
mod tests;

pub use overlay::{OverlayOptions, paint_canvas_overlay, paint_document_overlay};
pub use tracks::TrackGeometry;
pub use transport::{ActionSpec, TimelineAction, action_spec, perform};

/// Longest clock step fed to the transport in one frame, so a stalled UI
/// does not jump the playhead.
const MAX_TICK: f64 = 0.25;

/// An inline message under the timeline: a refused edit or a hint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notice {
    /// The core's stable error id, `None` for hints.
    pub msg_key: Option<&'static str>,
    /// The text shown, in the locale it was raised in.
    pub text: String,
}

impl Notice {
    /// A refused edit, worded in `locale`.
    #[must_use]
    pub fn error<T>(locale: Locale, error: &AnimEditorError<T>) -> Self {
        Self {
            msg_key: Some(error.msg_key()),
            text: text::describe_error(locale, error),
        }
    }

    /// A hint (for example why a shortcut did nothing) from an already
    /// localized reason.
    #[must_use]
    pub fn hint(reason: &str) -> Self {
        let mut chars = reason.chars();
        let text = chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(chars).collect::<String>()
        });
        Self {
            msg_key: None,
            text: format!("{text}."),
        }
    }
}

/// A preset chosen for an element the document has no outline for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresetRequest<T> {
    pub target: T,
    pub phase: fm_animation::Phase,
    pub preset: fm_animation::AnimationPreset,
    /// The preset's length; a shorter phase is lengthened to it.
    pub frames: u32,
}

/// UI state the caller keeps between frames, next to its [`AnimDocument`].
#[derive(Clone, Debug)]
pub struct AnimationTimelineState<T> {
    /// Whether the timeline owns the keyboard shortcuts. Set when the
    /// operator presses inside the widget, cleared when they press
    /// elsewhere.
    pub keyboard_active: bool,
    /// The property row `K` and the value field act on (Advanced mode).
    pub active_row: Option<TrackKey<T>>,
    /// Why the animation may not be edited now (catalog English, for
    /// example a padlock or a missing permission). While set, every edit
    /// control is disabled with this reason; playback, scrubbing,
    /// selection, copy, and the mode switch keep working.
    pub lock_reason: Option<&'static str>,
    /// Presets of elements the document has no outline for (so no
    /// geometry): `None` disables them, `Some(Ok(()))` turns each choice
    /// into a [`PresetRequest`] for the caller to apply elsewhere (the
    /// server knows the geometry), and `Some(Err(reason))` disables them
    /// with that reason (catalog English).
    pub outside_presets: Option<Result<(), &'static str>>,
    /// Preset choices for elements without an outline, oldest first; the
    /// caller takes them.
    pub preset_requests: Vec<PresetRequest<T>>,
    /// The preset the pointer rests on in an open Simple-mode picker this
    /// frame (previewed by the caller, not applied); `None` otherwise.
    pub preset_hover: Option<PresetRequest<T>>,
    /// The properties the Advanced-mode "Animate property" menu offers, in
    /// display order: every property for title elements, all but colour for
    /// scene layers (GX-011 stage 5).
    pub properties: &'static [Property],
    pub(crate) notice: Option<Notice>,
    last_time: Option<f64>,
    length_edit: Option<u32>,
    pub(crate) value_edit: Option<f64>,
    track_width: f32,
    /// The language of the last frame drawn, for notices raised between
    /// frames.
    locale: Locale,
}

impl<T> Default for AnimationTimelineState<T> {
    fn default() -> Self {
        Self {
            keyboard_active: false,
            active_row: None,
            lock_reason: None,
            outside_presets: None,
            preset_requests: Vec::new(),
            preset_hover: None,
            properties: &Property::ALL,
            notice: None,
            last_time: None,
            length_edit: None,
            value_edit: None,
            track_width: 0.0,
            locale: Locale::ENGLISH,
        }
    }
}

impl<T> AnimationTimelineState<T> {
    /// The inline notice currently shown, if any.
    #[must_use]
    pub const fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// The language the timeline was last drawn in (English before the
    /// first frame).
    #[must_use]
    pub const fn locale(&self) -> Locale {
        self.locale
    }

    /// Shows `error` inline, worded in [`Self::locale`].
    pub fn show_error(&mut self, error: &AnimEditorError<T>) {
        self.notice = Some(Notice::error(self.locale, error));
    }

    pub fn clear_notice(&mut self) {
        self.notice = None;
    }
}

/// What one frame of the widget did, plus geometry for callers and tests.
#[derive(Clone, Debug)]
pub struct TimelineOutput {
    /// The widget's whole area.
    pub response: Response,
    /// Whether the draft changed this frame (edits, drags, undo/redo).
    pub changed: bool,
    /// Transport actions run this frame, by button or key.
    pub actions: Vec<TimelineAction>,
    /// Each transport control's rect, in bar order.
    pub transport: Vec<(TimelineAction, Rect)>,
    /// Each phase tab's rect.
    pub phase_tabs: Vec<(Phase, Rect)>,
    /// Ruler and row layout.
    pub tracks: TrackGeometry,
}

/// The animation timeline: header, transport, rows, inspector, and notice.
pub struct AnimationTimelineWidget<'a, T> {
    document: &'a mut AnimDocument<T>,
    state: &'a mut AnimationTimelineState<T>,
    locale: Option<Locale>,
}

impl<'a, T: Copy + Ord> AnimationTimelineWidget<'a, T> {
    /// A timeline worded in the locale the Studio shell set for the frame
    /// (English outside a shell).
    #[must_use]
    pub const fn new(
        document: &'a mut AnimDocument<T>,
        state: &'a mut AnimationTimelineState<T>,
    ) -> Self {
        Self {
            document,
            state,
            locale: None,
        }
    }

    /// Words the timeline in `locale` instead of the frame's locale.
    #[must_use]
    pub const fn locale(mut self, locale: Locale) -> Self {
        self.locale = Some(locale);
        self
    }

    /// Draws the timeline and applies the operator's input to the document.
    pub fn show(self, ui: &mut Ui) -> TimelineOutput {
        let Self {
            document,
            state,
            locale,
        } = self;
        let l = locale.unwrap_or_else(|| l10n::locale(ui.ctx()));
        state.locale = l;
        let generation = document.generation();
        let mut actions = Vec::new();

        tick(ui, document, state);
        for result in transport::handle_keys(ui, document, state) {
            match result {
                Ok(action) => {
                    actions.push(action);
                    if action != TimelineAction::Copy {
                        state.notice = None;
                    }
                }
                Err(error) => state.show_error(&error),
            }
        }

        let mut transport_rects = Vec::new();
        let mut phase_tabs = Vec::new();
        let inner = Frame::new()
            .fill(panel_color())
            .stroke(Stroke::new(line_width(), border_color()))
            .corner_radius(CornerRadius::same(crate::theme::RADIUS + 2))
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                header(ui, l, document, state, &mut phase_tabs);
                for (action, shift) in
                    transport::draw_transport(ui, l, document, state, &mut transport_rects)
                {
                    match perform(action, document, state, shift) {
                        Ok(()) => actions.push(action),
                        Err(error) => state.show_error(&error),
                    }
                }
                let geometry = tracks::draw(ui, l, document, state);
                state.track_width = geometry.track_width();
                if document.mode() == EditorMode::Advanced {
                    inspector::draw(ui, l, document, state, &geometry);
                }
                notice(ui, l, state);
                geometry
            });
        let response = inner.response;
        let geometry = inner.inner;

        // The timeline owns the keyboard after a press inside it, until a
        // press elsewhere.
        let (pressed, press) =
            ui.input(|input| (input.pointer.any_pressed(), input.pointer.interact_pos()));
        if pressed && let Some(position) = press {
            state.keyboard_active = response.rect.contains(position);
        }
        if document.transport().is_playing() {
            follow_playhead(document, &geometry);
            ui.ctx().request_repaint();
        }

        TimelineOutput {
            response,
            changed: document.generation() != generation,
            actions,
            transport: transport_rects,
            phase_tabs,
            tracks: geometry,
        }
    }
}

/// Advances playback by the frame's clock time.
fn tick<T: Copy + Ord>(
    ui: &Ui,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) {
    let now = ui.input(|input| input.time);
    if document.transport().is_playing()
        && let Some(last) = state.last_time
    {
        let elapsed = (now - last).clamp(0.0, MAX_TICK);
        document.tick(Duration::from_secs_f64(elapsed));
    }
    state.last_time = Some(now);
}

/// Keeps the playhead in view while playing.
fn follow_playhead<T: Copy + Ord>(document: &mut AnimDocument<T>, geometry: &TrackGeometry) {
    let width = geometry.track_width();
    let (first, last) = document.view().visible_range(width);
    let playhead = document.playhead();
    if playhead < first || playhead + 1 >= last {
        #[allow(clippy::cast_precision_loss)] // Frames stay below 2^24.
        document.view_mut().scroll_to(playhead as f32);
    }
}

/// Phase tabs, the hold segment, the phase length, and zoom-to-fit.
fn header<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
    tabs: &mut Vec<(Phase, Rect)>,
) {
    let idle = document.drag().is_none();
    let busy = l.text(Msg::AnimReasonDrag);
    let lock = state.lock_reason.map(|reason| l.tr(reason));
    ui.horizontal(|ui| {
        ui.label(section_label(l.text(Msg::AnimSection)))
            .on_hover_text(l.text(Msg::AnimSectionHover));
        ui.add_space(8.0);
        for phase in [Phase::In, Phase::Out] {
            if phase == Phase::Out {
                crate::theme::pill(ui, l.text(Msg::AnimPhaseHold), muted_color(), false)
                    .on_hover_text(l.text(Msg::AnimHoldHover));
            }
            let shown = document.phase() == phase;
            let frames = document.clip().phase_frames(phase);
            let tab = egui::Button::new(
                RichText::new(format!(
                    "{}  {}",
                    text::phase_label(l, phase),
                    l10n::frames(l, frames)
                ))
                .size(legible(11.0))
                .strong()
                .color(if shown {
                    text_color()
                } else {
                    text_dim_color()
                }),
            )
            .fill(if shown { ACCENT_FILL } else { widget_color() })
            .stroke(Stroke::new(
                line_width(),
                if shown {
                    accent_color().gamma_multiply(0.7)
                } else {
                    border_strong_color()
                },
            ))
            .min_size(Vec2::new(64.0, 22.0));
            let response = ui.add_enabled(idle, tab).hint_in(
                l,
                &format!(
                    "{}\n{}",
                    text::phase_hover(l, phase),
                    l.text(Msg::AnimPhaseTabClick)
                ),
                busy,
            );
            tabs.push((phase, response.rect));
            if response.clicked() {
                document.set_phase(phase);
                state.active_row = state
                    .active_row
                    .map(|row| TrackKey::new(row.target, phase, row.property));
                state.length_edit = None;
            }
        }
        ui.add_space(8.0);
        ui.label(crate::theme::field_label(l.text(Msg::AnimLength)));
        let mut length = state.length_edit.unwrap_or_else(|| document.phase_frames());
        let response = ui
            .add_enabled(
                idle && lock.is_none(),
                DragValue::new(&mut length)
                    .range(0..=MAX_PHASE_FRAMES)
                    .speed(0.25)
                    .suffix(" fr"),
            )
            .hint_in(l, l.text(Msg::AnimLengthHover), lock.unwrap_or(busy));
        if response.changed() {
            state.length_edit = Some(length);
        }
        if !(response.dragged() || response.has_focus())
            && let Some(length) = state.length_edit.take()
            && lock.is_none()
            && length != document.phase_frames()
            && let Err(error) = document.set_phase_duration(document.phase(), length)
        {
            state.show_error(&error);
        }
        let fit = ui
            .add_enabled(idle, crate::theme::small_button(l.text(Msg::AnimFit)))
            .hint_in(l, l.text(Msg::AnimFitHover), busy);
        if fit.clicked() {
            fit_view(document, state.track_width);
        }
    });
}

/// Zooms so the whole shown phase (plus a little margin) fits `width`.
#[allow(clippy::cast_precision_loss)] // Frames stay below 2^24.
pub fn fit_view<T: Copy + Ord>(document: &mut AnimDocument<T>, width: f32) {
    if width <= 1.0 {
        return;
    }
    let frames = document.phase_frames().max(1) as f32 * 1.08;
    let view = document.view_mut();
    view.set_frames_per_pixel((frames / width).clamp(MIN_FRAMES_PER_PIXEL, MAX_FRAMES_PER_PIXEL));
    view.scroll_to(0.0);
}

/// The inline notice with a dismiss key.
fn notice<T>(ui: &mut Ui, l: Locale, state: &mut AnimationTimelineState<T>) {
    let Some(notice) = state.notice.clone() else {
        return;
    };
    let color = if notice.msg_key.is_some() {
        ERROR
    } else {
        AMBER
    };
    let mut dismissed = false;
    Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(crate::theme::RADIUS))
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let label = ui.label(RichText::new(&notice.text).color(color));
                match notice.msg_key {
                    Some(key) => label
                        .on_hover_text(l.format(Msg::AnimRefusedHover, &[("key", Arg::Str(key))])),
                    None => label.on_hover_text(l.text(Msg::AnimHintHover)),
                };
                if ui
                    .add(crate::theme::close_key(12.0))
                    .on_hover_text(l.text(Msg::AnimDismissHover))
                    .clicked()
                {
                    dismissed = true;
                }
            });
        });
    if dismissed {
        state.notice = None;
    }
}
