//! Window chrome: the top status/menu bar, the message banner, and the
//! bottom status bar.

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Margin, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};
use fm_protocol::StreamRealizedState;

use crate::panels::latest_stream_sample;
use crate::theme::{
    ACCENT, AMBER, BORDER, BORDER_STRONG, ERROR, GRAPHITE, GRAPHITE_RAISED, MUTED, PREVIEW,
    PROGRAM, TEXT, TEXT_DIM, WIDGET_ACTIVE, WIDGET_HOVER, connection_color, mono, paint_lock_glyph,
    pill, status_dot,
};
use crate::toasts::{Toast, ToastKind};
use crate::{
    PadlockToggle, REDO_SHORTCUT, StudioConnectionStatus, StudioIntent, StudioUiState,
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

/// Formats seconds since the Unix epoch as a UTC wall-clock `HH:MM:SS`.
pub(crate) fn format_utc_clock(seconds_since_epoch: u64) -> String {
    let seconds = seconds_since_epoch % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

fn utc_now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// The top-bar edit group: the project padlock beside Undo and Redo keys,
/// each with its hover explanation and (when available) its intent.
fn edit_controls(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    let padlock = padlock_toggle(state);
    let summary = edit_lock_summary(state);
    let response = padlock_button(ui, padlock);
    let hover = format!(
        "{}\n{}",
        if padlock.locked {
            "Click to unlock project edits."
        } else {
            "Click to lock project edits; live switching stays available."
        },
        summary.detail()
    );
    let response = if padlock.enabled {
        response.on_hover_text(hover)
    } else {
        response.on_hover_text(format!("{hover}\nUnavailable: {}", padlock.reason))
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
                "Undo",
                UNDO_SHORTCUT,
                history.undo,
                history.undo_intent(),
                false,
            ),
            (
                "Redo",
                REDO_SHORTCUT,
                history.redo,
                history.redo_intent(),
                true,
            ),
        ] {
            if history_key(ui, side.enabled(), redo)
                .on_hover_text(side.hover(verb, shortcut))
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
                WIDGET_ACTIVE
            } else if hovered {
                WIDGET_HOVER
            } else {
                GRAPHITE
            },
        );
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, if hovered { BORDER_STRONG } else { BORDER }),
            StrokeKind::Inside,
        );
        let color = if !enabled {
            MUTED.gamma_multiply(0.55)
        } else if hovered {
            TEXT
        } else {
            TEXT_DIM
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
    match state.view.as_ref().map(|view| view.show_name.as_str()) {
        Some(name) => {
            let name = if name.is_empty() {
                "Untitled show"
            } else {
                name
            };
            // Long show names end in an ellipsis (full name on hover)
            // so the on-air badges and clock keep their place.
            let max_width = (ui.available_width() * 0.35).max(120.0);
            ui.scope(|ui| {
                ui.set_max_width(max_width);
                ui.add(
                    egui::Label::new(RichText::new(name).size(15.0).strong().color(TEXT))
                        .truncate(),
                );
            });
        }
        None => {
            ui.label(
                RichText::new("No synchronized show")
                    .size(15.0)
                    .color(MUTED),
            )
            .on_hover_text("The show name appears once project state is synchronized");
        }
    }
}

pub(crate) fn draw_top_bar(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        // Wordmark.
        ui.label(
            RichText::new("FREEMIX")
                .size(13.0)
                .strong()
                .extra_letter_spacing(2.5)
                .color(ACCENT),
        );
        ui.label(
            RichText::new("STUDIO")
                .size(10.5)
                .extra_letter_spacing(1.5)
                .color(MUTED),
        );
        divider(ui);
        show_name_label(ui, state);
        let status = state.connection_status;
        pill(ui, status.operator_label(), connection_color(status), false)
            .on_hover_text("Control connection lifecycle");
        if let Some(view) = &state.view {
            ui.label(
                mono(format!("REV {}", view.cursor.revision.get()))
                    .size(11.0)
                    .color(TEXT_DIM),
            )
            .on_hover_text("Replicated project revision");
        }
        if state.pending_commands > 0 {
            pill(
                ui,
                &format!("{} PENDING", state.pending_commands),
                AMBER,
                false,
            );
        }
        if state.view.is_some() {
            divider(ui);
            edit_controls(ui, state, intents);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let now = utc_now_seconds();
            ui.label(mono(format_utc_clock(now)).size(17.0).color(TEXT))
                .on_hover_text("Wall clock (UTC)");
            ui.label(RichText::new("UTC").size(10.0).color(MUTED));
            divider(ui);
            let air = on_air(state);
            let stream_color = if air.stream_live {
                PROGRAM
            } else if air.stream_desired {
                AMBER
            } else {
                MUTED
            };
            on_air_badge(ui, "LIVE", stream_color, air.stream_live).on_hover_text(
                if air.stream_live {
                    "A stream target reports realized Live"
                } else if air.stream_desired {
                    "A stream target is desired running but not reported live"
                } else {
                    "No stream target is desired running"
                },
            );
            on_air_badge(
                ui,
                "REC",
                if air.recording { PROGRAM } else { MUTED },
                air.recording,
            )
            .on_hover_text(if air.recording {
                "A recorder slot is desired recording"
            } else {
                "No recorder slot is desired recording"
            });
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
    let size = Vec2::new(104.0, 24.0);
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
                if hovered { WIDGET_HOVER } else { GRAPHITE },
                if hovered { BORDER_STRONG } else { BORDER },
                MUTED,
                TEXT_DIM,
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
            padlock.label(),
            FontId::new(10.5, FontFamily::Proportional),
            text.gamma_multiply(dim),
        );
    }
    response
}

fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 20.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, BORDER_STRONG);
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
            painter.rect_filled(rect, CornerRadius::same(4), GRAPHITE);
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
    let color = match toast.kind {
        ToastKind::Error => ERROR,
        ToastKind::Notice | ToastKind::Uncertain | ToastKind::Pending => AMBER,
        ToastKind::Success => PREVIEW,
    };
    let mut dismissed = false;
    let response = Frame::new()
        .fill(GRAPHITE_RAISED)
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
                    RichText::new(toast.kind.title())
                        .size(10.5)
                        .strong()
                        .extra_letter_spacing(1.0)
                        .color(color),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("×").size(15.0).color(TEXT_DIM))
                                .frame(false),
                        )
                        .on_hover_text("Dismiss; the status bar keeps the latest message")
                        .clicked()
                    {
                        dismissed = true;
                    }
                });
            });
            ui.add(egui::Label::new(RichText::new(&toast.message).size(12.0).color(TEXT)).wrap());
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
    painter.rect_filled(track, CornerRadius::same(1), BORDER);
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
pub(crate) fn draw_status_bar(ui: &mut Ui, state: &StudioUiState) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let status = state.connection_status;
        status_dot(ui, connection_color(status));
        ui.label(
            RichText::new(status.operator_label())
                .size(10.5)
                .color(connection_color(status)),
        );
        bar_divider(ui);
        ui.label(
            mono(format!("{} pending", state.pending_commands))
                .size(10.5)
                .color(if state.pending_commands > 0 {
                    AMBER
                } else {
                    MUTED
                }),
        );
        bar_divider(ui);
        if edit_lock_summary(state).project {
            ui.label(
                RichText::new("EDITS LOCKED")
                    .size(10.0)
                    .strong()
                    .extra_letter_spacing(0.6)
                    .color(AMBER),
            )
            .on_hover_text(edit_lock_summary(state).detail());
            bar_divider(ui);
        }
        let (message, color) = status_message(state);
        ui.add(egui::Label::new(RichText::new(message).size(10.5).color(color)).truncate());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(
                    "1–8 PVW   C CUT   F FADE   B FTB   ESC CANCEL T-BAR   ·   HELP › SHORTCUTS",
                )
                .size(10.0)
                .extra_letter_spacing(0.5)
                .color(MUTED),
            );
            if status.controls_enabled() {
                ui.label(RichText::new("KEYS").size(10.0).color(TEXT_DIM));
            }
        });
    });
}

fn bar_divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 12.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, BORDER);
}

/// Picks the most important one-line status message.
pub(crate) fn status_message(state: &StudioUiState) -> (String, Color32) {
    if let Some(error) = &state.error {
        return (format!("ERROR | {error}"), ERROR);
    }
    if let Some(uncertainty) = state.terminal_uncertainties.last() {
        return (
            format!(
                "{} command(s) with uncertain outcome; last {}",
                state.terminal_uncertainties.len(),
                uncertainty.command_id
            ),
            AMBER,
        );
    }
    if let Some(notice) = state.notice.as_ref().or(state.external_notice.as_ref()) {
        return (format!("NOTICE | {notice}"), AMBER);
    }
    let idle = match state.connection_status {
        StudioConnectionStatus::Ready => "Ready",
        StudioConnectionStatus::ProtocolMismatch => "Peer did not select the current protocol",
        StudioConnectionStatus::Failed => "Connection failed",
        StudioConnectionStatus::Disconnected => "No control connection",
        StudioConnectionStatus::Launching
        | StudioConnectionStatus::Connecting
        | StudioConnectionStatus::Synchronizing
        | StudioConnectionStatus::Backoff => "Waiting for synchronized state",
    };
    (idle.to_owned(), MUTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_clock_wraps_days() {
        assert_eq!(format_utc_clock(0), "00:00:00");
        assert_eq!(format_utc_clock(86_399), "23:59:59");
        assert_eq!(format_utc_clock(86_400 + 3_661), "01:01:01");
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
        assert_eq!(status_message(&state).0, "Ready");
        state.notice = Some("saved".to_owned());
        assert_eq!(status_message(&state).0, "NOTICE | saved");
        state
            .terminal_uncertainties
            .push(crate::TerminalUncertaintyNotice {
                command_id: "c1".to_owned(),
                received_command_id: "r1".to_owned(),
            });
        assert!(status_message(&state).0.contains("uncertain"));
        state.error = Some("boom".to_owned());
        assert_eq!(status_message(&state), ("ERROR | boom".to_owned(), ERROR));
    }
}
