//! Window chrome: the top status/menu bar, the message banner, and the
//! bottom status bar.

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Margin, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};
use fm_protocol::StreamRealizedState;

use crate::health::HealthItem;
use crate::l10n::{self, Arg, Locale, Msg, trailing_layout};
use crate::panels::latest_stream_sample;
use crate::theme::{
    AMBER, ERROR, PREVIEW, PROGRAM, accent_color, border_color, border_strong_color,
    connection_color, graphite_color, mono, muted_color, paint_lamp, paint_lock_glyph, pill,
    raised_color, status_dot, text_color, text_dim_color, widget_active_color, widget_hover_color,
};
use crate::toasts::{Toast, ToastKind};
use crate::{
    HistoryVerb, PadlockToggle, REDO_SHORTCUT, StudioConnectionStatus, StudioIntent, StudioUiState,
    UNDO_SHORTCUT, edit_history_availability, edit_lock_summary, padlock_toggle,
};

/// On-air indicator facts for the top bar, derived only from replicated state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct OnAir {
    /// At least one recorder slot's authoritative desired flag is set.
    pub(crate) recording: bool,
    /// At least one stream target reports a realized Live state.
    pub(crate) stream_live: bool,
    /// At least one stream target is desired running.
    pub(crate) stream_desired: bool,
}

pub(crate) fn on_air(state: &StudioUiState) -> OnAir {
    let live = |stream: &fm_ui_model::StreamStatus| {
        latest_stream_sample(state.stream_status.as_ref(), stream.target)
            .map_or(stream.realized, |sample| sample.realized)
            == StreamRealizedState::Live
    };
    OnAir {
        recording: state.recording_desired.iter().any(|desired| *desired),
        stream_live: state.streams.iter().any(live),
        stream_desired: state.streams.iter().any(|stream| stream.desired_running),
    }
}

/// Formats seconds since the Unix epoch as the UTC wall clock in the
/// locale's hour cycle (`HH:MM:SS` on the default 24-hour clock).
pub(crate) fn format_utc_clock(locale: Locale, seconds_since_epoch: u64) -> String {
    locale.time_of_day(seconds_since_epoch)
}

/// The clock tooltip: what the clock shows and today's UTC date.
pub(crate) fn utc_clock_hover(locale: Locale, seconds_since_epoch: u64) -> String {
    let days = i64::try_from(seconds_since_epoch / 86_400).unwrap_or(i64::MAX);
    locale.format(Msg::ClockHover, &[("date", Arg::Str(&locale.date(days)))])
}

pub(crate) fn utc_now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// The top-bar edit group: the project padlock beside Undo and Redo keys,
/// each with its hover explanation and (when available) its intent.
/// A `compact` padlock shows only its glyph; its hover still says whether
/// edits are locked.
fn edit_controls(
    ui: &mut Ui,
    state: &StudioUiState,
    compact: bool,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let padlock = padlock_toggle(state);
    let summary = edit_lock_summary(state);
    let response = padlock_button(ui, padlock, compact);
    let hover = format!(
        "{}\n{}",
        l.text(if padlock.locked {
            Msg::PadlockClickUnlock
        } else {
            Msg::PadlockClickLock
        }),
        summary.detail(l)
    );
    let response = if padlock.enabled {
        response.on_hover_text(hover)
    } else {
        response.on_hover_text(l10n::disabled_text(l, &hover, padlock.reason))
    };
    if response.clicked()
        && let Some(intent) = padlock.intent()
    {
        intents.push(intent);
    }
    let history = edit_history_availability(state);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (verb, shortcut, side, intent, redo) in [
            (
                HistoryVerb::Undo,
                UNDO_SHORTCUT,
                history.undo,
                history.undo_intent(),
                false,
            ),
            (
                HistoryVerb::Redo,
                REDO_SHORTCUT,
                history.redo,
                history.redo_intent(),
                true,
            ),
        ] {
            if history_key(ui, side.enabled(), redo)
                .on_hover_text(side.hover(l, verb, shortcut))
                .clicked()
                && let Some(intent) = intent
            {
                intents.push(intent);
            }
        }
    });
}

/// Width of the Undo and Redo keys.
const HISTORY_KEY_WIDTH: f32 = 28.0;

/// A compact Undo or Redo key with a painted turn-back arrow.
fn history_key(ui: &mut Ui, enabled: bool, redo: bool) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(HISTORY_KEY_WIDTH, 24.0), sense);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let hovered = enabled && response.hovered();
        painter.rect_filled(
            rect,
            CornerRadius::same(4),
            if response.is_pointer_button_down_on() && enabled {
                widget_active_color()
            } else if hovered {
                widget_hover_color()
            } else {
                graphite_color()
            },
        );
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(
                crate::theme::line_width(),
                if hovered {
                    border_strong_color()
                } else {
                    border_color()
                },
            ),
            StrokeKind::Inside,
        );
        let color = if !enabled {
            muted_color().gamma_multiply(0.55)
        } else if hovered {
            text_color()
        } else {
            text_dim_color()
        };
        paint_turn_arrow(painter, rect.center() + Vec2::new(0.0, 1.0), color, redo);
    }
    response
}

/// Paints a turn-back arrow: over the top and down on the left for Undo,
/// mirrored for Redo.
fn paint_turn_arrow(painter: &egui::Painter, center: egui::Pos2, color: Color32, redo: bool) {
    let radius = 5.5;
    let mirror = if redo { -1.0 } else { 1.0 };
    let points: Vec<egui::Pos2> = (0..=14)
        .map(|step| {
            #[allow(clippy::cast_precision_loss)] // fourteen arc steps
            let angle = std::f32::consts::PI - (step as f32 / 14.0) * 1.2 * std::f32::consts::PI;
            center + Vec2::new(mirror * radius * angle.cos(), -radius * angle.sin())
        })
        .collect();
    let tip_base = points[0];
    painter.add(egui::Shape::line(points, Stroke::new(1.6, color)));
    painter.add(egui::Shape::convex_polygon(
        vec![
            tip_base + Vec2::new(0.0, 3.5),
            tip_base + Vec2::new(-3.2, -1.0),
            tip_base + Vec2::new(3.2, -1.0),
        ],
        color,
        Stroke::NONE,
    ));
}

/// The show name, ellipsized to `max_width` (full name on hover) so the
/// on-air badges and the clock keep their place.
fn show_name_label(ui: &mut Ui, state: &StudioUiState, max_width: f32) {
    let l = l10n::locale(ui.ctx());
    match state.view.as_ref().map(|view| view.show_name.as_str()) {
        Some(name) => {
            let name = if name.is_empty() {
                l.text(Msg::UntitledShow)
            } else {
                name
            };
            ui.scope(|ui| {
                ui.set_max_width(max_width);
                ui.add(egui::Label::new(show_name_text(name)).truncate());
            });
        }
        None => {
            ui.scope(|ui| {
                ui.set_max_width(max_width);
                ui.add(
                    egui::Label::new(
                        RichText::new(l.text(Msg::NoSynchronizedShow))
                            .size(15.0)
                            .color(muted_color()),
                    )
                    .truncate(),
                )
                .on_hover_text(l.text(Msg::ShowNameHover));
            });
        }
    }
}

fn show_name_text(name: &str) -> RichText {
    RichText::new(name).size(15.0).strong().color(text_color())
}

fn wordmark() -> RichText {
    RichText::new("FREEMIX")
        .size(13.0)
        .strong()
        .extra_letter_spacing(2.5)
        .color(accent_color())
}

fn product_name() -> RichText {
    RichText::new("STUDIO")
        .size(crate::theme::legible(10.5))
        .extra_letter_spacing(1.5)
        .color(muted_color())
}

fn revision_text(l: Locale, revision: u64) -> RichText {
    mono(l.format(Msg::RevisionShort, &[("revision", Arg::Plain(revision))]))
        .size(crate::theme::legible(11.0))
        .color(text_dim_color())
}

fn clock_text(l: Locale, now: u64) -> RichText {
    mono(format_utc_clock(l, now))
        .size(17.0)
        .color(text_color())
}

fn utc_caption() -> RichText {
    RichText::new("UTC")
        .size(crate::theme::legible(10.0))
        .color(muted_color())
}

/// How much of the top bar is drawn. A narrow window sheds detail, least
/// needed first, so the edit keys and the on-air badges never overlap or
/// clip: the padlock drops its word (the glyph and its hover stay), then
/// the STUDIO word, the revision (the status bar and history keep it),
/// the UTC caption (the clock's hover says UTC), and the wordmark.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum BarFit {
    Full,
    PadlockGlyph,
    NoProduct,
    NoRevision,
    NoUtcCaption,
    NoWordmark,
}

impl BarFit {
    const ALL: [Self; 6] = [
        Self::Full,
        Self::PadlockGlyph,
        Self::NoProduct,
        Self::NoRevision,
        Self::NoUtcCaption,
        Self::NoWordmark,
    ];

    fn leading_spacing(self) -> f32 {
        if self == Self::Full {
            LEADING_SPACING
        } else {
            COMPACT_LEADING_SPACING
        }
    }
}

/// Spacing between the top bar's leading and trailing items; a bar that
/// sheds detail packs its leading items closer.
const LEADING_SPACING: f32 = 10.0;
const COMPACT_LEADING_SPACING: f32 = 7.0;
const TRAILING_SPACING: f32 = 8.0;
/// The narrowest the show name gets before the bar sheds more.
const MIN_SHOW_NAME: f32 = 96.0;
/// Least room between the leading and trailing groups.
const BAR_GAP: f32 = 12.0;
/// Width of an on-air badge.
const BADGE_WIDTH: f32 = 58.0;
/// Width of the padlock as a glyph-only key.
const PADLOCK_GLYPH_WIDTH: f32 = 28.0;

/// The widths the top bar's items take, measured in the console language.
struct BarWidths {
    wordmark: f32,
    product: f32,
    show_name: f32,
    pill: f32,
    revision: Option<f32>,
    pending: Option<f32>,
    padlock: Option<f32>,
    clock: f32,
    utc: f32,
}

fn text_width(ui: &Ui, text: RichText) -> f32 {
    egui::WidgetText::from(text)
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::FontSelection::Default,
        )
        .size()
        .x
}

impl BarWidths {
    fn measure(ui: &Ui, l: Locale, state: &StudioUiState, now: u64) -> Self {
        let pill = |text: &str| crate::theme::pill_width(ui, text);
        let show_name = state.view.as_ref().map_or_else(
            || {
                text_width(
                    ui,
                    RichText::new(l.text(Msg::NoSynchronizedShow)).size(15.0),
                )
            },
            |view| {
                let name = if view.show_name.is_empty() {
                    l.text(Msg::UntitledShow)
                } else {
                    view.show_name.as_str()
                };
                text_width(ui, show_name_text(name))
            },
        );
        Self {
            wordmark: text_width(ui, wordmark()),
            product: text_width(ui, product_name()),
            show_name,
            pill: pill(l.tr(state.connection_status.operator_label())),
            revision: state
                .view
                .as_ref()
                .map(|view| text_width(ui, revision_text(l, view.cursor.revision.get()))),
            pending: (state.pending_commands > 0).then(|| {
                pill(&l.format(
                    Msg::PendingPill,
                    &[("count", Arg::from(state.pending_commands))],
                ))
            }),
            padlock: state.view.as_ref().map(|_| padlock_width(ui, l)),
            clock: text_width(ui, clock_text(l, now)),
            utc: text_width(ui, utc_caption()),
        }
    }

    /// The leading group's width at `fit`, with the show name at `name`.
    fn leading(&self, fit: BarFit, name: f32) -> f32 {
        let mut items = vec![name, self.pill];
        if fit < BarFit::NoWordmark {
            items.extend([self.wordmark, 1.0]);
        }
        if fit < BarFit::NoProduct {
            items.push(self.product);
        }
        if fit < BarFit::NoRevision {
            items.extend(self.revision);
        }
        items.extend(self.pending);
        if let Some(padlock) = self.padlock {
            let padlock = if fit < BarFit::PadlockGlyph {
                padlock
            } else {
                PADLOCK_GLYPH_WIDTH
            };
            // The divider, the padlock, and the Undo and Redo keys.
            items.extend([1.0, padlock, 2.0 * HISTORY_KEY_WIDTH + 2.0]);
        }
        gaps(&items, fit.leading_spacing())
    }

    /// The trailing group's width at `fit`.
    fn trailing(&self, fit: BarFit) -> f32 {
        let mut items = vec![self.clock, 1.0, BADGE_WIDTH, BADGE_WIDTH];
        if fit < BarFit::NoUtcCaption {
            items.push(self.utc);
        }
        gaps(&items, TRAILING_SPACING)
    }

    /// The fullest bar that fits `width` with the whole show name, else
    /// the fullest that fits it shortened to [`MIN_SHOW_NAME`], else the
    /// most compact; with the most the show name may take.
    fn fit(&self, width: f32) -> (BarFit, f32) {
        let room = |fit| width - self.leading(fit, 0.0) - self.trailing(fit) - BAR_GAP;
        let fits = |least: f32| BarFit::ALL.into_iter().find(|fit| room(*fit) >= least);
        let fit = fits(self.show_name)
            .or_else(|| fits(MIN_SHOW_NAME))
            .unwrap_or(BarFit::NoWordmark);
        (
            fit,
            room(fit).max(MIN_SHOW_NAME.min(self.show_name)).max(24.0),
        )
    }
}

/// Items' widths plus the spacing between them.
fn gaps(items: &[f32], spacing: f32) -> f32 {
    #[allow(clippy::cast_precision_loss)] // a handful of items
    let between = items.len().saturating_sub(1) as f32 * spacing;
    items.iter().sum::<f32>() + between
}

pub(crate) fn draw_top_bar(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    let l = l10n::locale(ui.ctx());
    let now = utc_now_seconds();
    let rect = ui.available_rect_before_wrap();
    let widths = BarWidths::measure(ui, l, state, now);
    let (fit, name_width) = widths.fit(rect.width());
    let trailing = widths.trailing(fit);
    let (lead_rect, trail_rect) = {
        let split = (rect.width() - trailing - BAR_GAP).max(0.0);
        let near = rect.width() - trailing;
        match l.direction() {
            fm_ui_model::i18n::TextDirection::LeftToRight => (
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + split, rect.max.y)),
                egui::Rect::from_min_max(egui::pos2(rect.left() + near, rect.min.y), rect.max),
            ),
            fm_ui_model::i18n::TextDirection::RightToLeft => (
                egui::Rect::from_min_max(egui::pos2(rect.right() - split, rect.min.y), rect.max),
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - near, rect.max.y)),
            ),
        }
    };
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(lead_rect)
            .layout(l10n::leading_layout(l)),
        |ui| {
            // Whatever the estimate missed is clipped, never overlapped.
            ui.set_clip_rect(lead_rect.intersect(ui.clip_rect()));
            draw_leading(ui, state, fit, name_width, intents);
        },
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(trail_rect)
            .layout(trailing_layout(l)),
        |ui| draw_trailing(ui, state, fit, now),
    );
}

/// The top bar's reading-order side: the wordmark, the show, the
/// connection, the revision, pending commands, and the edit keys.
fn draw_leading(
    ui: &mut Ui,
    state: &StudioUiState,
    fit: BarFit,
    name_width: f32,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    ui.spacing_mut().item_spacing.x = fit.leading_spacing();
    if fit < BarFit::NoWordmark {
        ui.label(wordmark());
        if fit < BarFit::NoProduct {
            ui.label(product_name());
        }
        divider(ui);
    }
    show_name_label(ui, state, name_width);
    let status = state.connection_status;
    pill(
        ui,
        l.tr(status.operator_label()),
        connection_color(status),
        false,
    )
    .on_hover_text(l.text(Msg::ConnectionPillHover));
    if let Some(view) = &state.view
        && fit < BarFit::NoRevision
    {
        ui.label(revision_text(l, view.cursor.revision.get()))
            .on_hover_text(l.text(Msg::RevisionHover));
    }
    if state.pending_commands > 0 {
        pill(
            ui,
            &l.format(
                Msg::PendingPill,
                &[("count", Arg::from(state.pending_commands))],
            ),
            AMBER,
            false,
        );
    }
    if state.view.is_some() {
        divider(ui);
        edit_controls(ui, state, fit >= BarFit::PadlockGlyph, intents);
    }
}

/// The top bar's far side: the UTC clock and the on-air badges.
fn draw_trailing(ui: &mut Ui, state: &StudioUiState, fit: BarFit, now: u64) {
    let l = l10n::locale(ui.ctx());
    ui.spacing_mut().item_spacing.x = TRAILING_SPACING;
    ui.label(clock_text(l, now))
        .on_hover_text(utc_clock_hover(l, now));
    if fit < BarFit::NoUtcCaption {
        ui.label(utc_caption());
    }
    divider(ui);
    let air = on_air(state);
    let stream_color = if air.stream_live {
        PROGRAM
    } else if air.stream_desired {
        AMBER
    } else {
        muted_color()
    };
    on_air_badge(ui, l.text(Msg::Live), stream_color, air.stream_live).on_hover_text(l.text(
        if air.stream_live {
            Msg::LiveHoverLive
        } else if air.stream_desired {
            Msg::LiveHoverDesired
        } else {
            Msg::LiveHoverIdle
        },
    ));
    on_air_badge(
        ui,
        l.text(Msg::BadgeRec),
        if air.recording {
            PROGRAM
        } else {
            muted_color()
        },
        air.recording,
    )
    .on_hover_text(l.text(if air.recording {
        Msg::RecHoverRecording
    } else {
        Msg::RecHoverIdle
    }));
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(500));
}

/// The padlock's width with its word: one width for both states, so
/// toggling never shifts the bar, wide enough for the longer word in the
/// console language.
fn padlock_width(ui: &Ui, l: Locale) -> f32 {
    let font = FontId::new(10.5, FontFamily::Proportional);
    let label_width = [Msg::Locked, Msg::Unlocked]
        .map(|word| {
            ui.painter()
                .layout_no_wrap(l.text(word).to_owned(), font.clone(), text_color())
                .size()
                .x
        })
        .into_iter()
        .fold(0.0_f32, f32::max);
    (29.0 + label_width + 9.0).max(104.0)
}

/// The project edit padlock: a painted lock glyph plus its state. Locked
/// reads as an amber chip (a deliberate hold on edits, like a pending state);
/// unlocked is an unlit outline. A `compact` padlock is the glyph alone.
/// Clicks are reported only when toggling is available.
fn padlock_button(ui: &mut Ui, padlock: PadlockToggle, compact: bool) -> egui::Response {
    let l = l10n::locale(ui.ctx());
    let font = FontId::new(10.5, FontFamily::Proportional);
    let width = if compact {
        PADLOCK_GLYPH_WIDTH
    } else {
        padlock_width(ui, l)
    };
    let size = Vec2::new(width, 24.0);
    let sense = if padlock.enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered() && padlock.enabled;
        let painter = ui.painter();
        let (fill, stroke, glyph, text) = if padlock.locked {
            (
                AMBER.gamma_multiply(if hovered { 0.24 } else { 0.16 }),
                AMBER.gamma_multiply(0.75),
                AMBER,
                AMBER,
            )
        } else {
            (
                if hovered {
                    widget_hover_color()
                } else {
                    graphite_color()
                },
                if hovered {
                    border_strong_color()
                } else {
                    border_color()
                },
                muted_color(),
                text_dim_color(),
            )
        };
        let dim = if padlock.enabled { 1.0 } else { 0.55 };
        painter.rect_filled(rect, CornerRadius::same(4), fill);
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, stroke.gamma_multiply(dim)),
            StrokeKind::Inside,
        );
        let glyph_centre = if compact {
            rect.center()
        } else {
            rect.left_center() + Vec2::new(15.0, 0.0)
        };
        paint_lock_glyph(
            painter,
            glyph_centre,
            1.0,
            glyph.gamma_multiply(dim),
            padlock.locked,
        );
        if !compact {
            painter.text(
                rect.left_center() + Vec2::new(29.0, 0.5),
                Align2::LEFT_CENTER,
                l.tr(padlock.label()),
                font,
                text.gamma_multiply(dim),
            );
        }
    }
    let label = l.tr(padlock.label()).to_owned();
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, padlock.enabled, &label)
    });
    response
}

fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 20.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, border_strong_color());
}

/// Broadcast-style on-air badge: solid red with a lamp when lit, outlined otherwise.
fn on_air_badge(ui: &mut Ui, label: &str, color: Color32, lit: bool) -> egui::Response {
    let size = Vec2::new(BADGE_WIDTH, 24.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if lit {
            painter.rect_filled(rect, CornerRadius::same(4), color);
        } else {
            painter.rect_filled(rect, CornerRadius::same(4), graphite_color());
            painter.rect_stroke(
                rect,
                CornerRadius::same(4),
                Stroke::new(1.0, color.gamma_multiply(0.6)),
                StrokeKind::Inside,
            );
        }
        let lamp = rect.left_center() + Vec2::new(12.0, 0.0);
        painter.circle_filled(
            lamp,
            4.0,
            if lit {
                Color32::WHITE
            } else {
                color.gamma_multiply(0.5)
            },
        );
        painter.text(
            rect.center() + Vec2::new(7.0, 0.0),
            Align2::CENTER_CENTER,
            label,
            FontId::new(12.0, FontFamily::Proportional),
            if lit { Color32::WHITE } else { color },
        );
    }
    response
}

/// Draws one toast card; returns `true` when the operator dismissed it.
pub(crate) fn draw_toast(ui: &mut Ui, toast: &Toast) -> bool {
    let l = l10n::locale(ui.ctx());
    let color = match toast.kind {
        ToastKind::Error => ERROR,
        ToastKind::Notice | ToastKind::Uncertain | ToastKind::Pending => AMBER,
        ToastKind::Success => PREVIEW,
    };
    let mut dismissed = false;
    let response = Frame::new()
        .fill(raised_color())
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.55)))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin {
            left: 14,
            right: 8,
            top: 8,
            bottom: 10,
        })
        .shadow(egui::Shadow {
            offset: [0, 4],
            blur: 14,
            spread: 0,
            color: Color32::from_black_alpha(140),
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(l.tr(toast.kind.title()))
                        .size(crate::theme::legible(10.5))
                        .strong()
                        .extra_letter_spacing(1.0)
                        .color(color),
                );
                ui.with_layout(trailing_layout(l), |ui| {
                    if ui
                        .add(crate::theme::close_key(15.0))
                        .on_hover_text(l.text(Msg::ToastDismissHover))
                        .clicked()
                    {
                        dismissed = true;
                    }
                });
            });
            let message = crate::toasts::localize_notice(l, &toast.message);
            let label = ui.add(
                egui::Label::new(RichText::new(message).size(12.0).color(text_color())).wrap(),
            );
            if let Some(detail) = &toast.detail {
                label.on_hover_text(detail);
            }
        })
        .response;
    let rect = response.rect;
    let painter = ui.painter();
    // Severity stripe and remaining-lifetime bar.
    painter.rect_filled(
        egui::Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
        CornerRadius {
            nw: 6,
            sw: 6,
            ne: 0,
            se: 0,
        },
        color,
    );
    #[allow(clippy::cast_precision_loss)] // lifetimes are a few thousand ms
    let fraction = toast.remaining_ms as f32 / toast.lifetime_ms.max(1) as f32;
    let track = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + 14.0, rect.max.y - 4.0),
        egui::pos2(rect.max.x - 8.0, rect.max.y - 2.0),
    );
    painter.rect_filled(track, CornerRadius::same(1), border_color());
    painter.rect_filled(
        egui::Rect::from_min_size(
            track.min,
            Vec2::new(track.width() * fraction, track.height()),
        ),
        CornerRadius::same(1),
        color.gamma_multiply(0.7),
    );
    dismissed
}

/// One-line bottom status bar: connection, last result, and key hints.
pub(crate) fn draw_status_bar(ui: &mut Ui, state: &StudioUiState, health: &[HealthItem]) {
    let l = l10n::locale(ui.ctx());
    // The trailing cluster is laid out first so the leading message
    // truncates into whatever width remains.
    ui.with_layout(trailing_layout(l), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for item in health.iter().rev() {
            health_lamp(ui, item);
        }
        bar_divider(ui);
        let status = state.connection_status;
        let hints = l.text(Msg::StatusBarKeyHints);
        let hint_width = ui
            .painter()
            .layout_no_wrap(
                hints.to_owned(),
                egui::FontId::proportional(crate::theme::legible(10.0)),
                muted_color(),
            )
            .size()
            .x;
        // Keep room for the connection, pending, and message on the leading side.
        if ui.available_width() - hint_width > STATUS_BAR_LEADING_WIDTH {
            ui.label(
                RichText::new(hints)
                    .size(crate::theme::legible(10.0))
                    .extra_letter_spacing(0.5)
                    .color(muted_color()),
            );
            if status.controls_enabled() {
                ui.label(
                    RichText::new(l.text(Msg::StatusBarKeys))
                        .size(crate::theme::legible(10.0))
                        .color(text_dim_color()),
                );
            }
        } else {
            ui.label(
                RichText::new(l.text(Msg::StatusBarHelpHint))
                    .size(crate::theme::legible(10.0))
                    .extra_letter_spacing(0.5)
                    .color(muted_color()),
            )
            .on_hover_text(hints);
        }
        ui.with_layout(l10n::leading_layout(l), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            status_dot(ui, connection_color(status));
            ui.label(
                RichText::new(l.tr(status.operator_label()))
                    .size(crate::theme::legible(10.5))
                    .color(connection_color(status)),
            );
            bar_divider(ui);
            ui.label(
                mono(l.format(
                    Msg::PendingCount,
                    &[("count", Arg::from(state.pending_commands))],
                ))
                .size(crate::theme::legible(10.5))
                .color(if state.pending_commands > 0 {
                    AMBER
                } else {
                    muted_color()
                }),
            );
            bar_divider(ui);
            if edit_lock_summary(state).project {
                ui.label(
                    RichText::new(l.text(Msg::EditsLocked))
                        .size(crate::theme::legible(10.0))
                        .strong()
                        .extra_letter_spacing(0.6)
                        .color(AMBER),
                )
                .on_hover_text(edit_lock_summary(state).detail(l));
                bar_divider(ui);
            }
            let (message, color) = status_message(l, state);
            ui.add(
                egui::Label::new(
                    RichText::new(message)
                        .size(crate::theme::legible(10.5))
                        .color(color),
                )
                .truncate(),
            );
        });
    });
}

/// Width the status bar keeps for its leading connection, pending count,
/// and message before it shortens the key hints to `? SHORTCUTS`.
const STATUS_BAR_LEADING_WIDTH: f32 = 420.0;

/// One health lamp: a small status lamp and its label in the lamp's tone.
fn health_lamp(ui: &mut Ui, item: &HealthItem) {
    let font = FontId::new(10.0, FontFamily::Proportional);
    let color = if item.tone.lit() {
        item.tone.color()
    } else {
        text_dim_color()
    };
    let galley = ui.painter().layout_no_wrap(item.label.clone(), font, color);
    let size = Vec2::new(galley.size().x + 14.0, 18.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        paint_lamp(
            painter,
            egui::pos2(rect.min.x + 4.0, rect.center().y),
            3.0,
            item.tone,
        );
        painter.galley(
            egui::pos2(rect.min.x + 12.0, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
    }
    response.on_hover_text(&item.hover);
}

fn bar_divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 12.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, border_color());
}

/// Picks the most important one-line status message.
pub(crate) fn status_message(l: Locale, state: &StudioUiState) -> (String, Color32) {
    if let Some(error) = &state.error {
        // A refusal the console words itself replaces its own line of the
        // error; anything else joined onto that error still shows.
        if let Some((refusal, rest)) = state
            .command_refusals
            .iter()
            .rev()
            .filter(|refusal| refusal.is_localized())
            .find_map(|refusal| Some((refusal, refusal.remainder_of(error)?)))
        {
            let mut message = refusal.text(l, state.view.as_ref());
            if let Some(rest) = rest {
                message.push_str("; ");
                message.push_str(&crate::l10n::host_message(l, &rest));
            }
            return (
                l.format(Msg::StatusError, &[("message", Arg::Str(&message))]),
                ERROR,
            );
        }
        return (
            l.format(
                Msg::StatusError,
                &[("message", Arg::Str(&crate::l10n::host_message(l, error)))],
            ),
            ERROR,
        );
    }
    if let Some(uncertainty) = state.terminal_uncertainties.last() {
        return (
            l.format(
                Msg::StatusUncertain,
                &[
                    ("count", Arg::from(state.terminal_uncertainties.len())),
                    ("command", Arg::Str(&uncertainty.command_id)),
                ],
            ),
            AMBER,
        );
    }
    if let Some(notice) = state.notice.as_ref().or(state.external_notice.as_ref()) {
        return (
            l.format(
                Msg::StatusNotice,
                &[(
                    "message",
                    Arg::Str(&crate::toasts::localize_notice(l, notice)),
                )],
            ),
            AMBER,
        );
    }
    let idle = match state.connection_status {
        StudioConnectionStatus::Ready => Msg::StatusIdleReady,
        StudioConnectionStatus::ProtocolMismatch => Msg::StatusIdleProtocol,
        StudioConnectionStatus::Failed => Msg::StatusIdleFailed,
        StudioConnectionStatus::Disconnected => Msg::StatusIdleDisconnected,
        StudioConnectionStatus::Launching
        | StudioConnectionStatus::Connecting
        | StudioConnectionStatus::Synchronizing
        | StudioConnectionStatus::Backoff => Msg::StatusIdleWaiting,
    };
    (l.text(idle).to_owned(), muted_color())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_clock_wraps_days() {
        assert_eq!(format_utc_clock(Locale::ENGLISH, 0), "00:00:00");
        assert_eq!(format_utc_clock(Locale::ENGLISH, 86_399), "23:59:59");
        assert_eq!(
            format_utc_clock(Locale::ENGLISH, 86_400 + 3_661),
            "01:01:01"
        );
        let twelve = Locale::ENGLISH.with_hour_cycle(fm_ui_model::i18n::HourCycle::H12);
        assert_eq!(format_utc_clock(twelve, 13 * 3_600 + 5), "1:00:05\u{a0}PM");
    }

    #[test]
    fn clock_hover_names_the_utc_date_in_the_locale() {
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        // 2026-10-14T12:00:00Z
        let now = 1_791_979_200;
        assert_eq!(
            utc_clock_hover(Locale::ENGLISH, now),
            "Wall clock (UTC) · Oct 14, 2026"
        );
        assert_eq!(
            utc_clock_hover(spanish, now),
            "Reloj de pared (UTC) · 14 oct 2026"
        );
    }

    #[test]
    fn status_message_is_localized_with_plural_counts() {
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        assert_eq!(status_message(spanish, &state).0, "Listo");
        state.notice = Some("Queued 1 command(s) in operator FIFO".to_owned());
        assert_eq!(
            status_message(spanish, &state).0,
            "AVISO | 1 comando en la cola del operador"
        );
        state.notice = Some("Queued 3 command(s) in operator FIFO".to_owned());
        assert_eq!(
            status_message(Locale::ENGLISH, &state).0,
            "NOTICE | 3 commands queued in the operator FIFO"
        );
        state
            .terminal_uncertainties
            .push(crate::TerminalUncertaintyNotice {
                command_id: "c9".to_owned(),
                received_command_id: "r9".to_owned(),
            });
        assert_eq!(
            status_message(spanish, &state).0,
            "1 comando con resultado incierto; el último c9"
        );
    }

    #[test]
    fn on_air_uses_desired_recording_and_reported_stream_state() {
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        assert_eq!(on_air(&state), OnAir::default());
        state.recording_desired = [false, true];
        assert!(on_air(&state).recording);
        state.streams.push(fm_ui_model::StreamStatus {
            target: core::num::NonZeroU128::new(4).unwrap(),
            name: "HLS".to_owned(),
            desired_running: true,
            realized: StreamRealizedState::Starting,
            detail: None,
        });
        let air = on_air(&state);
        assert!(air.stream_desired);
        assert!(!air.stream_live);
        state.streams[0].realized = StreamRealizedState::Live;
        assert!(on_air(&state).stream_live);
    }

    #[test]
    fn status_message_prefers_errors_then_uncertainty_then_notices() {
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        assert_eq!(status_message(Locale::ENGLISH, &state).0, "Ready");
        state.notice = Some("saved".to_owned());
        assert_eq!(status_message(Locale::ENGLISH, &state).0, "NOTICE | saved");
        state
            .terminal_uncertainties
            .push(crate::TerminalUncertaintyNotice {
                command_id: "c1".to_owned(),
                received_command_id: "r1".to_owned(),
            });
        assert!(
            status_message(Locale::ENGLISH, &state)
                .0
                .contains("uncertain")
        );
        state.error = Some("boom".to_owned());
        assert_eq!(
            status_message(Locale::ENGLISH, &state),
            ("ERROR | boom".to_owned(), ERROR)
        );
    }
}
