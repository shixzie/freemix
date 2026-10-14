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

fn utc_now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// The top-bar edit group: the project padlock beside Undo and Redo keys,
/// each with its hover explanation and (when available) its intent.
fn edit_controls(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    let l = l10n::locale(ui.ctx());
    let padlock = padlock_toggle(state);
    let summary = edit_lock_summary(state);
    let response = padlock_button(ui, padlock);
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

/// A compact Undo or Redo key with a painted turn-back arrow.
fn history_key(ui: &mut Ui, enabled: bool, redo: bool) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(28.0, 24.0), sense);
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
                1.0,
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

/// The show name, ellipsized so on-air badges and the clock keep their place.
fn show_name_label(ui: &mut Ui, state: &StudioUiState) {
    let l = l10n::locale(ui.ctx());
    match state.view.as_ref().map(|view| view.show_name.as_str()) {
        Some(name) => {
            let name = if name.is_empty() {
                l.text(Msg::UntitledShow)
            } else {
                name
            };
            // Long show names end in an ellipsis (full name on hover)
            // so the on-air badges and clock keep their place.
            let max_width = (ui.available_width() * 0.35).max(120.0);
            ui.scope(|ui| {
                ui.set_max_width(max_width);
                ui.add(
                    egui::Label::new(RichText::new(name).size(15.0).strong().color(text_color()))
                        .truncate(),
                );
            });
        }
        None => {
            ui.label(
                RichText::new(l.text(Msg::NoSynchronizedShow))
                    .size(15.0)
                    .color(muted_color()),
            )
            .on_hover_text(l.text(Msg::ShowNameHover));
        }
    }
}

pub(crate) fn draw_top_bar(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    let l = l10n::locale(ui.ctx());
    l10n::leading_row(ui, l, |ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        // Wordmark.
        ui.label(
            RichText::new("FREEMIX")
                .size(13.0)
                .strong()
                .extra_letter_spacing(2.5)
                .color(accent_color()),
        );
        ui.label(
            RichText::new("STUDIO")
                .size(10.5)
                .extra_letter_spacing(1.5)
                .color(muted_color()),
        );
        divider(ui);
        show_name_label(ui, state);
        let status = state.connection_status;
        pill(
            ui,
            l.tr(status.operator_label()),
            connection_color(status),
            false,
        )
        .on_hover_text(l.text(Msg::ConnectionPillHover));
        if let Some(view) = &state.view {
            ui.label(
                mono(l.format(
                    Msg::RevisionShort,
                    &[("revision", Arg::Plain(view.cursor.revision.get()))],
                ))
                .size(11.0)
                .color(text_dim_color()),
            )
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
            edit_controls(ui, state, intents);
        }

        ui.with_layout(trailing_layout(l), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let now = utc_now_seconds();
            ui.label(
                mono(format_utc_clock(l, now))
                    .size(17.0)
                    .color(text_color()),
            )
            .on_hover_text(utc_clock_hover(l, now));
            ui.label(RichText::new("UTC").size(10.0).color(muted_color()));
            divider(ui);
            let air = on_air(state);
            let stream_color = if air.stream_live {
                PROGRAM
            } else if air.stream_desired {
                AMBER
            } else {
                muted_color()
            };
            on_air_badge(ui, l.text(Msg::Live), stream_color, air.stream_live).on_hover_text(
                l.text(if air.stream_live {
                    Msg::LiveHoverLive
                } else if air.stream_desired {
                    Msg::LiveHoverDesired
                } else {
                    Msg::LiveHoverIdle
                }),
            );
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
        });
    });
}

/// The project edit padlock: a painted lock glyph plus its state. Locked
/// reads as an amber chip (a deliberate hold on edits, like a pending state);
/// unlocked is an unlit outline. Clicks are reported only when toggling is
/// available.
fn padlock_button(ui: &mut Ui, padlock: PadlockToggle) -> egui::Response {
    let l = l10n::locale(ui.ctx());
    let font = FontId::new(10.5, FontFamily::Proportional);
    // One width for both states, so toggling never shifts the bar; wide
    // enough for the longer word in the console language.
    let label_width = [Msg::Locked, Msg::Unlocked]
        .map(|word| {
            ui.painter()
                .layout_no_wrap(l.text(word).to_owned(), font.clone(), text_color())
                .size()
                .x
        })
        .into_iter()
        .fold(0.0_f32, f32::max);
    let size = Vec2::new((29.0 + label_width + 9.0).max(104.0), 24.0);
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
        paint_lock_glyph(
            painter,
            rect.left_center() + Vec2::new(15.0, 0.0),
            1.0,
            glyph.gamma_multiply(dim),
            padlock.locked,
        );
        painter.text(
            rect.left_center() + Vec2::new(29.0, 0.5),
            Align2::LEFT_CENTER,
            l.tr(padlock.label()),
            font,
            text.gamma_multiply(dim),
        );
    }
    response
}

fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 20.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, border_strong_color());
}

/// Broadcast-style on-air badge: solid red with a lamp when lit, outlined otherwise.
fn on_air_badge(ui: &mut Ui, label: &str, color: Color32, lit: bool) -> egui::Response {
    let size = Vec2::new(58.0, 24.0);
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
                        .size(10.5)
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
            ui.add(egui::Label::new(RichText::new(message).size(12.0).color(text_color())).wrap());
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
                egui::FontId::proportional(10.0),
                muted_color(),
            )
            .size()
            .x;
        // Keep room for the connection, pending, and message on the leading side.
        if ui.available_width() - hint_width > STATUS_BAR_LEADING_WIDTH {
            ui.label(
                RichText::new(hints)
                    .size(10.0)
                    .extra_letter_spacing(0.5)
                    .color(muted_color()),
            );
            if status.controls_enabled() {
                ui.label(
                    RichText::new(l.text(Msg::StatusBarKeys))
                        .size(10.0)
                        .color(text_dim_color()),
                );
            }
        } else {
            ui.label(
                RichText::new(l.text(Msg::StatusBarHelpHint))
                    .size(10.0)
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
                    .size(10.5)
                    .color(connection_color(status)),
            );
            bar_divider(ui);
            ui.label(
                mono(l.format(
                    Msg::PendingCount,
                    &[("count", Arg::from(state.pending_commands))],
                ))
                .size(10.5)
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
                        .size(10.0)
                        .strong()
                        .extra_letter_spacing(0.6)
                        .color(AMBER),
                )
                .on_hover_text(edit_lock_summary(state).detail(l));
                bar_divider(ui);
            }
            let (message, color) = status_message(l, state);
            ui.add(egui::Label::new(RichText::new(message).size(10.5).color(color)).truncate());
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
        return (
            l.format(Msg::StatusError, &[("message", Arg::Str(error))]),
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
