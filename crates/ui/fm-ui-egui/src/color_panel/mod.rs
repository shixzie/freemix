//! The colour grading panel's `egui` widgets over the headless
//! [`GradeDocument`] core (SW-019).
//!
//! [`ColorPanelWidget`] draws, top to bottom:
//!
//! * a header with the panel title, the Simple/Advanced toggle, and the
//!   action keys (bypass, A/B compare, undo/redo, copy/paste, reset all),
//!   each with hover help naming its shortcut and a reason when disabled;
//! * on the left, the controls. Simple mode shows a gallery of looks, each
//!   a live thumbnail of this picture with the look applied, then the basic
//!   sliders (exposure, contrast, saturation, temperature, tint) and auto
//!   balance. Advanced mode shows the sliders (plus the contrast pivot),
//!   the Lift/Gamma/Gain/Offset colour wheels with their master sliders,
//!   the curve editor with RGB and hue/luma curve tabs, and the LUT slot
//!   ([`controls`], [`view`]);
//! * on the right, the graded preview with Graded/Wipe/Side-by-side
//!   compare and a draggable wipe line, then the scopes strip (waveform,
//!   parade, vectorscope, histogram);
//! * an inline notice for refused edits, worded from
//!   [`GradeEditorError::msg_key`] ([`text`]).
//!
//! Every control explains itself on hover in the Studio locale, every drag
//! is relative with Shift for fine control and is one undo step, and every
//! slider, wheel, and the wipe line resets on double-click. Previews are
//! graded locally through [`fm_ui_model::grade_editor::GradePreview`]
//! (cached by grade) and recomputed latest-wins with a short debounce while
//! dragging. The widget edits the document directly and never talks to a
//! server.

use core::time::Duration;
use std::sync::Arc;

use egui::{
    CornerRadius, Event, Frame, Key, Margin, Modifiers, Rect, Response, RichText, Stroke, Ui, Vec2,
};
use fm_grade::{CubeLut, WheelKind};
use fm_ui_model::grade_editor::{
    AbSlot, CompareMode, CurveTarget, GradeDocument, GradeEditorError, GradeParam, GradePreview,
    GradeSection, RecomputeScheduler, RgbaImage, ScopeKind, Scopes, ValueDrag, WheelDrag,
};
use fm_ui_model::i18n::{Arg, Locale, Msg};

use crate::gating::Hint;
use crate::l10n;
use crate::theme::{
    ACCENT_FILL, AMBER, ERROR, accent_color, border_color, border_strong_color, legible,
    line_width, panel_color, section_label, text_color, widget_color,
};

pub mod controls;
pub mod text;
pub mod view;

#[cfg(test)]
mod tests;

pub use controls::SliderLayout;

/// Largest preview the panel grades, in pixels.
pub const PREVIEW_MAX: (u32, u32) = (480, 270);
/// Size of each look thumbnail's source, in pixels.
pub const THUMB_MAX: (u32, u32) = (112, 63);

/// Which controls the panel shows.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum PanelMode {
    #[default]
    Simple,
    Advanced,
}

/// One action key in the panel header.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GradeAction {
    ToggleMode,
    Bypass,
    CompareAb,
    Undo,
    Redo,
    Copy,
    Paste,
    ResetAll,
}

impl GradeAction {
    /// Every action key, in header order.
    pub const ALL: [Self; 8] = [
        Self::ToggleMode,
        Self::Bypass,
        Self::CompareAb,
        Self::Undo,
        Self::Redo,
        Self::Copy,
        Self::Paste,
        Self::ResetAll,
    ];

    /// The keyboard shortcut in `locale`'s key names, if any.
    #[must_use]
    pub fn shortcut(self, locale: Locale) -> Option<&'static str> {
        Some(match self {
            Self::ToggleMode => "M",
            Self::Bypass => "B",
            Self::CompareAb => "A",
            Self::Undo => "Ctrl+Z",
            Self::Redo => locale.text(Msg::AnimKeyRedo),
            Self::Copy => "Ctrl+C",
            Self::Paste => "Ctrl+V",
            Self::ResetAll => return None,
        })
    }
}

/// The shortcut that cycles the compare modes.
pub const COMPARE_SHORTCUT: &str = "W";

/// How one action key looks and reads right now, in one locale.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionSpec {
    pub label: String,
    /// What the key does, without the shortcut.
    pub action: String,
    /// Why the key is disabled; `None` while enabled.
    pub disabled_reason: Option<&'static str>,
    /// Whether the key is a toggle that is on.
    pub active: bool,
    pub locale: Locale,
}

impl ActionSpec {
    /// The hover text: the action with its shortcut.
    #[must_use]
    pub fn hover(&self, action: GradeAction) -> String {
        match action.shortcut(self.locale) {
            Some(key) => l10n::with_key(self.locale, &self.action, key),
            None => self.action.clone(),
        }
    }
}

/// The current look and availability of `action`, worded in `locale`.
#[must_use]
pub fn action_spec(
    action: GradeAction,
    document: &GradeDocument,
    state: &ColorPanelState,
    locale: Locale,
) -> ActionSpec {
    let l = locale;
    let dragging = document
        .gesture_active()
        .then(|| l.text(Msg::AnimReasonDrag));
    // A locked panel edits nothing; viewing (mode, copy) stays available.
    let busy = state.lock_reason.or(dragging);
    let (label, text, reason, active): (String, String, Option<&'static str>, bool) = match action {
        GradeAction::ToggleMode => match state.mode {
            PanelMode::Simple => (
                l.text(Msg::AnimAdvanced).to_owned(),
                l.text(Msg::GradeToAdvancedHover).to_owned(),
                dragging,
                false,
            ),
            PanelMode::Advanced => (
                l.text(Msg::AnimSimple).to_owned(),
                l.text(Msg::GradeToSimpleHover).to_owned(),
                dragging,
                true,
            ),
        },
        GradeAction::Bypass => (
            l.text(Msg::GradeBypass).to_owned(),
            l.text(if document.is_bypassed() {
                Msg::GradeBypassOnHover
            } else {
                Msg::GradeBypassOffHover
            })
            .to_owned(),
            busy,
            document.is_bypassed(),
        ),
        GradeAction::CompareAb => {
            let slot = slot_name(document.active_slot());
            let compared = document.slot(document.active_slot().other()).is_some();
            (
                if compared {
                    format!("{} · {slot}", l.text(Msg::GradeAb))
                } else {
                    l.text(Msg::GradeAb).to_owned()
                },
                l.format(Msg::GradeAbHover, &[("slot", Arg::Str(slot))]),
                busy,
                compared,
            )
        }
        GradeAction::Undo => (
            l.text(Msg::Undo).to_owned(),
            l.text(Msg::GradeUndoHover).to_owned(),
            busy.or_else(|| (!document.can_undo()).then(|| l.text(Msg::GradeReasonNothingToUndo))),
            false,
        ),
        GradeAction::Redo => (
            l.text(Msg::Redo).to_owned(),
            l.text(Msg::GradeRedoHover).to_owned(),
            busy.or_else(|| (!document.can_redo()).then(|| l.text(Msg::GradeReasonNothingToRedo))),
            false,
        ),
        GradeAction::Copy => (
            l.text(Msg::AnimCopy).to_owned(),
            l.text(Msg::GradeCopyHover).to_owned(),
            None,
            false,
        ),
        GradeAction::Paste => (
            l.text(Msg::AnimPaste).to_owned(),
            l.text(Msg::GradePasteHover).to_owned(),
            busy.or_else(|| {
                document
                    .clipboard()
                    .is_none()
                    .then(|| l.text(Msg::GradeReasonNothingCopied))
            }),
            false,
        ),
        GradeAction::ResetAll => (
            l.text(Msg::GradeResetAll).to_owned(),
            l.text(Msg::GradeResetAllHover).to_owned(),
            busy.or_else(|| {
                GradeSection::All
                    .is_default(document.grade())
                    .then(|| l.text(Msg::GradeReasonNeutral))
            }),
            false,
        ),
    };
    ActionSpec {
        label,
        action: text,
        disabled_reason: reason,
        active,
        locale,
    }
}

const fn slot_name(slot: AbSlot) -> &'static str {
    match slot {
        AbSlot::A => "A",
        AbSlot::B => "B",
    }
}

/// Runs `action`.
///
/// # Errors
///
/// The core's refusal, which leaves the document unchanged.
pub fn perform(
    action: GradeAction,
    document: &mut GradeDocument,
    state: &mut ColorPanelState,
) -> Result<(), GradeEditorError> {
    match action {
        GradeAction::ToggleMode => {
            if document.gesture_active() {
                return Err(GradeEditorError::GestureActive);
            }
            state.mode = match state.mode {
                PanelMode::Simple => PanelMode::Advanced,
                PanelMode::Advanced => PanelMode::Simple,
            };
            Ok(())
        }
        GradeAction::Bypass => document.toggle_bypass(),
        GradeAction::CompareAb => document.toggle_ab(),
        GradeAction::Undo => document.undo(),
        GradeAction::Redo => document.redo(),
        GradeAction::Copy => {
            document.copy();
            Ok(())
        }
        GradeAction::Paste => document.paste(),
        GradeAction::ResetAll => document.reset_section(GradeSection::All),
    }
}

/// An inline message under the panel: a refused edit or a hint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notice {
    /// The core's stable error id, `None` for hints.
    pub msg_key: Option<&'static str>,
    pub text: String,
}

/// What the preview shows: a source picture, a draft generation, and the
/// set of supplied LUTs.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PreviewKey {
    pub source: u64,
    pub generation: u64,
    pub luts: u64,
}

/// A drag in progress inside the panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PanelDrag {
    Slider(GradeParam, ValueDrag),
    Wheel(WheelKind, WheelDrag),
    Curve(controls::CurveDrag),
    Wipe,
}

/// UI state the caller keeps between frames, next to its
/// [`GradeDocument`]: the view settings, the source picture and its graded
/// previews, and the textures drawn from them.
pub struct ColorPanelState {
    pub mode: PanelMode,
    /// Whether the panel owns the keyboard shortcuts. Set when the operator
    /// presses inside the panel, cleared when they press elsewhere.
    pub keyboard_active: bool,
    pub compare: CompareMode,
    /// Wipe line position, `0..=1` from the left.
    pub wipe: f32,
    pub scope: ScopeKind,
    /// The curve shown in the curve editor.
    pub curve: CurveTarget,
    /// LUT asset URIs the operator may pick from.
    pub lut_choices: Vec<String>,
    /// Display names of LUT asset URIs; others show their file name.
    pub lut_names: std::collections::BTreeMap<String, String>,
    /// Why the grade cannot be edited (catalog English, worded in the
    /// panel's locale when shown); `None` while editing is allowed. A
    /// locked panel still previews, compares, and copies.
    pub lock_reason: Option<&'static str>,
    pub(crate) notice: Option<Notice>,
    pub(crate) drag: Option<PanelDrag>,
    pub(crate) preview: GradePreview,
    pub(crate) thumbs: GradePreview,
    pub(crate) scheduler: RecomputeScheduler<PreviewKey>,
    pub(crate) lut_epoch: u64,
    pub(crate) view: view::ViewCache,
    locale: Locale,
}

impl Default for ColorPanelState {
    fn default() -> Self {
        Self {
            mode: PanelMode::Simple,
            keyboard_active: false,
            compare: CompareMode::Off,
            wipe: 0.5,
            scope: ScopeKind::Waveform,
            curve: CurveTarget::RGB[0],
            lut_choices: Vec::new(),
            lut_names: std::collections::BTreeMap::new(),
            lock_reason: None,
            notice: None,
            drag: None,
            preview: GradePreview::default(),
            thumbs: GradePreview::new(16),
            scheduler: RecomputeScheduler::default(),
            lut_epoch: 0,
            view: view::ViewCache::default(),
            locale: Locale::ENGLISH,
        }
    }
}

impl ColorPanelState {
    /// A state previewing `image`.
    #[must_use]
    pub fn with_source(image: &RgbaImage) -> Self {
        let mut state = Self::default();
        state.set_source(image);
        state
    }

    /// Replaces the picture the panel previews (scaled down to
    /// [`PREVIEW_MAX`]; looks use a [`THUMB_MAX`] copy).
    pub fn set_source(&mut self, image: &RgbaImage) {
        let preview = image.fit_within(PREVIEW_MAX.0, PREVIEW_MAX.1);
        self.thumbs
            .set_source(preview.fit_within(THUMB_MAX.0, THUMB_MAX.1));
        self.preview.set_source(preview);
        self.scheduler.reset();
    }

    /// Supplies the parsed LUT for `asset_uri` to the previews.
    pub fn set_lut(&mut self, asset_uri: &str, lut: Arc<CubeLut>) {
        self.preview.set_lut(asset_uri, Arc::clone(&lut));
        self.thumbs.set_lut(asset_uri, lut);
        self.lut_epoch += 1;
    }

    /// The preview engine (for its source, LUTs, and cache statistics).
    #[must_use]
    pub const fn preview(&self) -> &GradePreview {
        &self.preview
    }

    /// The key of the picture the preview shows now.
    #[must_use]
    pub fn shown(&self) -> Option<PreviewKey> {
        self.view.shown_key()
    }

    /// The scopes of the picture the preview shows now.
    #[must_use]
    pub fn scopes(&self) -> Option<&Scopes> {
        self.view.scopes()
    }

    /// The inline notice currently shown, if any.
    #[must_use]
    pub const fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// The language the panel was last drawn in (English before the first
    /// frame).
    #[must_use]
    pub const fn locale(&self) -> Locale {
        self.locale
    }

    /// Shows `error` inline, worded in [`Self::locale`].
    pub fn show_error(&mut self, error: &GradeEditorError) {
        self.notice = Some(Notice {
            msg_key: Some(error.msg_key()),
            text: text::describe_error(self.locale, error),
        });
    }

    fn show_hint(&mut self, reason: &str) {
        let mut chars = reason.chars();
        let text = chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(chars).collect::<String>()
        });
        self.notice = Some(Notice {
            msg_key: None,
            text: format!("{text}."),
        });
    }

    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    /// The preview key for `document` now.
    #[must_use]
    pub const fn key(&self, document: &GradeDocument) -> PreviewKey {
        PreviewKey {
            source: self.preview.source_id(),
            generation: document.generation(),
            luts: self.lut_epoch,
        }
    }
}

/// Rects of the panel's controls, for callers and tests.
#[derive(Clone, Debug, Default)]
pub struct PanelRects {
    pub actions: Vec<(GradeAction, Rect)>,
    pub looks: Vec<(&'static str, Rect)>,
    pub sliders: Vec<(GradeParam, Rect)>,
    pub wheels: Vec<(WheelKind, Rect)>,
    pub curve_tabs: Vec<(CurveTarget, Rect)>,
    pub curve: Option<Rect>,
    pub compare_tabs: Vec<(CompareMode, Rect)>,
    pub scope_tabs: Vec<(ScopeKind, Rect)>,
    pub preview: Option<Rect>,
    pub wipe_handle: Option<Rect>,
    pub auto_balance: Option<Rect>,
}

/// What one frame of the widget did, plus geometry.
#[derive(Clone, Debug)]
pub struct ColorPanelOutput {
    /// The widget's whole area.
    pub response: Response,
    /// Whether the draft changed this frame.
    pub changed: bool,
    /// Header actions run this frame, by key or click.
    pub actions: Vec<GradeAction>,
    pub rects: PanelRects,
}

/// The colour grading panel.
pub struct ColorPanelWidget<'a> {
    document: &'a mut GradeDocument,
    state: &'a mut ColorPanelState,
    locale: Option<Locale>,
    titled: bool,
}

impl<'a> ColorPanelWidget<'a> {
    /// A panel worded in the locale the Studio shell set for the frame
    /// (English outside a shell).
    #[must_use]
    pub const fn new(document: &'a mut GradeDocument, state: &'a mut ColorPanelState) -> Self {
        Self {
            document,
            state,
            locale: None,
            titled: true,
        }
    }

    /// Leaves out the panel's own "Colour grade" title, for a host whose
    /// window or tab already shows it.
    #[must_use]
    pub const fn untitled(mut self) -> Self {
        self.titled = false;
        self
    }

    /// Words the panel in `locale` instead of the frame's locale.
    #[must_use]
    pub const fn locale(mut self, locale: Locale) -> Self {
        self.locale = Some(locale);
        self
    }

    /// Draws the panel and applies the operator's input to the document.
    pub fn show(self, ui: &mut Ui) -> ColorPanelOutput {
        let Self {
            document,
            state,
            locale,
            titled,
        } = self;
        let l = locale.unwrap_or_else(|| l10n::locale(ui.ctx()));
        state.locale = l;
        let generation = document.generation();
        let mut actions = Vec::new();
        let mut rects = PanelRects::default();

        for result in handle_keys(ui, document, state) {
            match result {
                Ok(action) => {
                    actions.push(action);
                    state.notice = None;
                }
                Err(error) => state.show_error(&error),
            }
        }

        let inner = Frame::new()
            .fill(panel_color())
            .stroke(Stroke::new(line_width(), border_color()))
            .corner_radius(CornerRadius::same(crate::theme::RADIUS + 2))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                for action in header(ui, l, titled, document, state, &mut rects) {
                    match perform(action, document, state) {
                        Ok(()) => {
                            actions.push(action);
                            state.notice = None;
                        }
                        Err(error) => state.show_error(&error),
                    }
                }
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 16.0;
                    let left = match state.mode {
                        PanelMode::Simple => 360.0,
                        PanelMode::Advanced => 520.0,
                    };
                    ui.vertical(|ui| {
                        ui.set_width(left);
                        match state.mode {
                            PanelMode::Simple => simple(ui, l, document, state, left, &mut rects),
                            PanelMode::Advanced => {
                                advanced(ui, l, document, state, left, &mut rects);
                            }
                        }
                    });
                    ui.vertical(|ui| {
                        let width = 440.0;
                        ui.set_width(width);
                        view::refresh(ui.ctx(), document, state);
                        view::preview(ui, l, document, state, width, &mut rects);
                        view::scopes(ui, l, state, width, &mut rects);
                    });
                });
                notice(ui, l, state);
            });
        let response = inner.response;

        // A drag whose widget vanished still ends as one undo step.
        if document.gesture_active() && !ui.input(|input| input.pointer.any_down()) {
            controls::finish_drag(document, state);
        }
        let (pressed, press) =
            ui.input(|input| (input.pointer.any_pressed(), input.pointer.interact_pos()));
        if pressed && let Some(position) = press {
            state.keyboard_active = response.rect.contains(position);
        }
        if let Some(deadline) = state.scheduler.next_deadline() {
            let now = Duration::from_secs_f64(ui.input(|input| input.time).max(0.0));
            ui.ctx().request_repaint_after(deadline.saturating_sub(now));
        }

        ColorPanelOutput {
            response,
            changed: document.generation() != generation,
            actions,
            rects,
        }
    }
}

/// Title, mode toggle, and action keys; returns the keys clicked.
fn header(
    ui: &mut Ui,
    l: Locale,
    titled: bool,
    document: &GradeDocument,
    state: &ColorPanelState,
    rects: &mut PanelRects,
) -> Vec<GradeAction> {
    let mut clicked = Vec::new();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if titled {
            ui.label(section_label(l.text(Msg::GradeTitle)))
                .on_hover_text(l.text(Msg::GradeTitleHover));
            ui.add_space(10.0);
        }
        for action in GradeAction::ALL {
            if matches!(
                action,
                GradeAction::Bypass | GradeAction::Undo | GradeAction::Copy | GradeAction::ResetAll
            ) {
                ui.separator();
            }
            let spec = action_spec(action, document, state, l);
            let button = egui::Button::new(RichText::new(&spec.label).size(legible(11.0)).color(
                if action == GradeAction::Bypass && spec.active {
                    AMBER
                } else {
                    text_color()
                },
            ))
            .fill(if spec.active {
                ACCENT_FILL
            } else {
                widget_color()
            })
            .stroke(Stroke::new(
                line_width(),
                if spec.active {
                    accent_color().gamma_multiply(0.7)
                } else {
                    border_strong_color()
                },
            ))
            .min_size(Vec2::new(30.0, 22.0));
            let response = ui
                .add_enabled(spec.disabled_reason.is_none(), button)
                .hint_in(l, &spec.hover(action), spec.disabled_reason.unwrap_or(""));
            rects.actions.push((action, response.rect));
            if response.clicked() {
                clicked.push(action);
            }
        }
    });
    clicked
}

/// Simple mode: looks, the basic sliders, and auto balance.
fn simple(
    ui: &mut Ui,
    l: Locale,
    document: &mut GradeDocument,
    state: &mut ColorPanelState,
    width: f32,
    rects: &mut PanelRects,
) {
    view::looks(ui, l, document, state, width, rects);
    ui.add_space(4.0);
    basics(
        ui,
        l,
        document,
        state,
        &GradeParam::SIMPLE,
        (width, 1),
        rects,
    );
    let picture = state.preview.source().is_some();
    let auto = ui
        .add_enabled(
            picture && state.lock_reason.is_none(),
            crate::theme::small_button(l.text(Msg::GradeAutoBalance)),
        )
        .hint_in(
            l,
            l.text(Msg::GradeAutoBalanceHover),
            state
                .lock_reason
                .unwrap_or_else(|| l.text(Msg::GradeReasonNoPicture)),
        );
    rects.auto_balance = Some(auto.rect);
    if auto.clicked()
        && let Some(source) = state.preview.source().cloned()
        && let Some(balance) = fm_ui_model::grade_editor::neutral_white_balance(&source)
        && let Err(error) = document.edit(|grade| {
            grade.white_balance = balance;
            Ok(())
        })
    {
        state.show_error(&error);
    }
}

/// The basic sliders in `columns` columns under a section label.
fn basics(
    ui: &mut Ui,
    l: Locale,
    document: &mut GradeDocument,
    state: &mut ColorPanelState,
    params: &[GradeParam],
    (width, columns): (f32, usize),
    rects: &mut PanelRects,
) {
    ui.label(section_label(l.text(Msg::GradeBasics)))
        .on_hover_text(l.text(Msg::GradeBasicsHover));
    #[allow(clippy::cast_precision_loss)] // One or two columns.
    let column_width = (width - 16.0 * (columns as f32 - 1.0)) / columns as f32;
    for row in params.chunks(columns) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            for &param in row {
                let response = controls::slider(
                    ui,
                    l,
                    document,
                    state,
                    param,
                    (column_width, SliderLayout::FULL),
                    None,
                );
                rects.sliders.push((param, response.rect));
            }
        });
    }
}

/// Advanced mode: sliders, wheels, curves, and the LUT slot.
fn advanced(
    ui: &mut Ui,
    l: Locale,
    document: &mut GradeDocument,
    state: &mut ColorPanelState,
    width: f32,
    rects: &mut PanelRects,
) {
    basics(
        ui,
        l,
        document,
        state,
        &[
            GradeParam::Exposure,
            GradeParam::Temperature,
            GradeParam::Contrast,
            GradeParam::Tint,
            GradeParam::Pivot,
            GradeParam::Saturation,
        ],
        (width, 2),
        rects,
    );
    ui.add_space(4.0);
    ui.label(section_label(l.text(Msg::GradeWheels)))
        .on_hover_text(l.text(Msg::GradeWheelsHover));
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let size = (width - 3.0 * 12.0) / 4.0;
        for kind in fm_ui_model::grade_editor::WHEEL_KINDS {
            let rect = controls::wheel(ui, l, document, state, kind, size);
            rects.wheels.push((kind, rect));
        }
    });
    ui.add_space(4.0);
    ui.label(section_label(l.text(Msg::GradeCurves)))
        .on_hover_text(l.text(Msg::GradeCurvesHover));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        for target in CurveTarget::ALL {
            if target == CurveTarget::HUE[0] {
                ui.add_space(8.0);
            }
            let response = controls::tab(
                ui,
                state.curve == target,
                text::curve_name(l, target),
                &format!(
                    "{}\n{}",
                    text::curve_hover(l, target),
                    l.text(Msg::GradeCurveShowHover)
                ),
            );
            rects.curve_tabs.push((target, response.rect));
            if response.clicked() {
                state.curve = target;
            }
        }
    });
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let target = state.curve;
        let rect = controls::curve_editor(ui, l, document, state, target, Vec2::new(300.0, 200.0));
        rects.curve = Some(rect);
        ui.vertical(|ui| {
            let side = width - 300.0 - 12.0;
            ui.set_width(side);
            let section = match target {
                CurveTarget::Rgb(channel) => GradeSection::Curve(channel),
                CurveTarget::Hue(kind) => GradeSection::HueCurve(kind),
            };
            let flat = section.is_default(document.grade());
            let reset = ui
                .add_enabled(
                    !flat && state.lock_reason.is_none(),
                    crate::theme::small_button(l.text(Msg::GradeCurveReset)),
                )
                .hint_in(
                    l,
                    &l.format(
                        Msg::GradeCurveResetHover,
                        &[("curve", Arg::Str(text::curve_name(l, target)))],
                    ),
                    state
                        .lock_reason
                        .unwrap_or_else(|| l.text(Msg::GradeReasonDefault)),
                );
            if reset.clicked()
                && let Err(error) = document.reset_section(section)
            {
                state.show_error(&error);
            }
            ui.add_space(6.0);
            view::lut_slot(ui, l, document, state, side, rects);
        });
    });
}

/// The inline notice with a dismiss key.
fn notice(ui: &mut Ui, l: Locale, state: &mut ColorPanelState) {
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

#[derive(Clone, Copy, Debug)]
enum Shortcut {
    Action(GradeAction),
    CycleCompare,
    Escape,
}

/// Keys handled while the panel owns the keyboard. Consumed keys do not
/// reach the rest of the console.
fn handle_keys(
    ui: &mut Ui,
    document: &mut GradeDocument,
    state: &mut ColorPanelState,
) -> Vec<Result<GradeAction, GradeEditorError>> {
    let mut results = Vec::new();
    if !state.keyboard_active || ui.ctx().egui_wants_keyboard_input() {
        return results;
    }
    if state.lock_reason.is_some() {
        // Keys that would edit explain the lock like the disabled keys do.
        state.keyboard_active = false;
        return results;
    }
    let shortcuts = ui.input_mut(|input| {
        let mut shortcuts = Vec::new();
        if input.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)
            || input.consume_key(Modifiers::COMMAND, Key::Y)
        {
            shortcuts.push(Shortcut::Action(GradeAction::Redo));
        }
        if input.consume_key(Modifiers::COMMAND, Key::Z) {
            shortcuts.push(Shortcut::Action(GradeAction::Undo));
        }
        if input.consume_key(Modifiers::COMMAND, Key::C) {
            shortcuts.push(Shortcut::Action(GradeAction::Copy));
        }
        if input.consume_key(Modifiers::COMMAND, Key::V) {
            shortcuts.push(Shortcut::Action(GradeAction::Paste));
        }
        input.events.retain(|event| match event {
            Event::Copy => {
                shortcuts.push(Shortcut::Action(GradeAction::Copy));
                false
            }
            Event::Paste(_) => {
                shortcuts.push(Shortcut::Action(GradeAction::Paste));
                false
            }
            _ => true,
        });
        for (key, shortcut) in [
            (Key::M, Shortcut::Action(GradeAction::ToggleMode)),
            (Key::B, Shortcut::Action(GradeAction::Bypass)),
            (Key::A, Shortcut::Action(GradeAction::CompareAb)),
            (Key::W, Shortcut::CycleCompare),
            (Key::Escape, Shortcut::Escape),
        ] {
            while input.consume_key(Modifiers::NONE, key) {
                shortcuts.push(shortcut);
            }
        }
        shortcuts
    });
    for shortcut in shortcuts {
        match shortcut {
            Shortcut::Action(action) => {
                // A key for a disabled control explains itself with the
                // same reason the button's hover gives.
                if let Some(reason) =
                    action_spec(action, document, state, state.locale()).disabled_reason
                {
                    state.show_hint(reason);
                    continue;
                }
                results.push(perform(action, document, state).map(|()| action));
            }
            Shortcut::CycleCompare => {
                let modes = CompareMode::ALL;
                let index = modes
                    .iter()
                    .position(|mode| *mode == state.compare)
                    .unwrap_or(0);
                state.compare = modes[(index + 1) % modes.len()];
            }
            Shortcut::Escape => {
                if document.gesture_active() {
                    state.drag = None;
                    let _ = document.cancel_gesture();
                }
            }
        }
    }
    results
}
