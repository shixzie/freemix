//! Centered status card over the monitors whenever controls are not Ready.

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Rect, Stroke, StrokeKind, Ui, Vec2, pos2,
};

use crate::l10n::{self, Arg, Locale, Msg};
use crate::theme::{
    AMBER, ERROR, border_strong_color, muted_color, raised_color, text_color, text_dim_color,
};
use crate::{StudioConnectionStatus, StudioUiState};

/// Visual tone of the connection card.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CardTone {
    /// Work in progress (connecting, synchronizing, retrying).
    Busy,
    /// Terminal failure that needs operator attention.
    Error,
    /// Idle without a connection.
    Idle,
}

/// Content of the connection card.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConnectionCard {
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) tone: CardTone,
    /// The last connection error while retrying, when one is known.
    pub(crate) last_error: Option<String>,
}

/// Returns the card to show, or `None` when the console is Ready with state.
pub(crate) fn connection_card(l: Locale, state: &StudioUiState) -> Option<ConnectionCard> {
    // A terminal failure already shows its error as the detail.
    let last_error = match state.connection_status {
        StudioConnectionStatus::Failed => None,
        _ => state.error.clone(),
    };
    let card = |title: Msg, detail: String, tone| {
        Some(ConnectionCard {
            title: l.text(title).to_owned(),
            detail,
            tone,
            last_error: last_error.clone(),
        })
    };
    match state.connection_status {
        StudioConnectionStatus::Ready if state.view.is_some() => None,
        StudioConnectionStatus::Ready | StudioConnectionStatus::Synchronizing => card(
            Msg::CardSynchronizing,
            l.text(Msg::CardSynchronizingDetail).to_owned(),
            CardTone::Busy,
        ),
        StudioConnectionStatus::Launching => card(
            Msg::CardStarting,
            l.text(Msg::CardStartingDetail).to_owned(),
            CardTone::Busy,
        ),
        StudioConnectionStatus::Connecting => card(
            Msg::CardConnecting,
            l.text(Msg::CardConnectingDetail).to_owned(),
            CardTone::Busy,
        ),
        StudioConnectionStatus::Backoff => card(
            Msg::CardReconnecting,
            state.reconnect.map_or_else(
                || l.text(Msg::CardReconnectWaiting).to_owned(),
                |reconnect| {
                    l.format(
                        Msg::CardReconnectAttempt,
                        &[
                            ("attempt", Arg::from(reconnect.attempt)),
                            ("delay", Arg::Str(&l.seconds_tenths(reconnect.delay_ms))),
                        ],
                    )
                },
            ),
            CardTone::Busy,
        ),
        StudioConnectionStatus::Disconnected => card(
            Msg::CardDisconnected,
            l.text(Msg::CardDisconnectedDetail).to_owned(),
            CardTone::Idle,
        ),
        StudioConnectionStatus::Failed => card(
            Msg::StatusIdleFailed,
            state
                .error
                .clone()
                .unwrap_or_else(|| l.text(Msg::CardFailedDetail).to_owned()),
            CardTone::Error,
        ),
        StudioConnectionStatus::ProtocolMismatch => card(
            Msg::CardProtocol,
            l.text(Msg::CardProtocolDetail).to_owned(),
            CardTone::Error,
        ),
    }
}

/// Paints the card centered in `area` on a foreground layer.
#[allow(clippy::too_many_lines)] // one painter pass for the whole card
pub(crate) fn paint_connection_card(ui: &Ui, area: Rect, card: &ConnectionCard) {
    let color = match card.tone {
        CardTone::Busy => AMBER,
        CardTone::Error => ERROR,
        CardTone::Idle => muted_color(),
    };
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("studio-connection-card"),
    ));
    let painter = painter.with_clip_rect(area);
    let l = l10n::locale(ui.ctx());
    let width = area.width().clamp(240.0, 480.0);
    let detail = painter.layout(
        card.detail.clone(),
        FontId::new(12.5, FontFamily::Proportional),
        text_dim_color(),
        width - 48.0,
    );
    let error = card.last_error.as_ref().map(|error| {
        let mut job = egui::text::LayoutJob::simple(
            l.format(Msg::CardLastError, &[("error", Arg::Str(error))]),
            FontId::new(11.0, FontFamily::Monospace),
            ERROR,
            width - 48.0,
        );
        job.wrap.max_rows = 3;
        painter.layout_job(job)
    });
    let footer = painter.layout(
        l.text(Msg::CardControlsDisabled).to_owned(),
        FontId::new(11.0, FontFamily::Proportional),
        muted_color(),
        width - 48.0,
    );
    let error_height = error.as_ref().map_or(0.0, |error| error.size().y + 10.0);
    let height = 92.0 + detail.size().y + error_height + footer.size().y;
    let rect = Rect::from_center_size(area.center(), Vec2::new(width, height));
    // Dim the wells behind so the card reads as modal state, not content.
    painter.rect_filled(area, CornerRadius::same(4), Color32::from_black_alpha(110));
    painter.rect_filled(
        rect.translate(Vec2::new(0.0, 4.0)),
        CornerRadius::same(8),
        Color32::from_black_alpha(90),
    );
    painter.rect_filled(rect, CornerRadius::same(8), raised_color());
    painter.rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(crate::theme::line_width(), border_strong_color()),
        StrokeKind::Inside,
    );
    painter.rect_filled(
        Rect::from_min_size(rect.min, Vec2::new(rect.width(), 3.0)),
        CornerRadius {
            nw: 8,
            ne: 8,
            sw: 0,
            se: 0,
        },
        color,
    );
    let title_y = rect.min.y + 32.0;
    let title = crate::theme::elided(
        &painter,
        &card.title,
        FontId::new(18.0, FontFamily::Proportional),
        text_color(),
        width - 64.0,
    );
    let lamp = pos2(rect.min.x + 26.0, title_y);
    if card.tone == CardTone::Busy {
        // A rotating arc driven by egui's frame time.
        let time = ui.input(|input| input.time);
        #[allow(clippy::cast_possible_truncation)]
        let start = (time * 4.0) as f32;
        let points: Vec<_> = (0..=16)
            .map(|step| {
                #[allow(clippy::cast_precision_loss)]
                let angle = start + step as f32 / 16.0 * std::f32::consts::PI * 1.4;
                lamp + Vec2::angled(angle) * 7.0
            })
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(2.0, color)));
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(50));
    } else {
        painter.circle_filled(lamp, 6.0, color);
    }
    painter.galley(
        Align2::LEFT_CENTER
            .anchor_size(pos2(rect.min.x + 44.0, title_y), title.size())
            .min,
        title,
        text_color(),
    );
    let mut y = title_y + 22.0;
    let detail_height = detail.size().y;
    painter.galley(pos2(rect.min.x + 24.0, y), detail, text_dim_color());
    y += detail_height + 10.0;
    if let Some(error) = error {
        let box_rect = Rect::from_min_size(
            pos2(rect.min.x + 18.0, y - 4.0),
            Vec2::new(width - 36.0, error.size().y + 8.0),
        );
        painter.rect_filled(box_rect, CornerRadius::same(4), ERROR.gamma_multiply(0.08));
        let error_height = error.size().y;
        painter.galley(pos2(rect.min.x + 24.0, y), error, ERROR);
        y += error_height + 10.0;
    }
    painter.line_segment(
        [pos2(rect.min.x + 18.0, y), pos2(rect.max.x - 18.0, y)],
        Stroke::new(1.0, crate::theme::faded(border_strong_color(), 0.6)),
    );
    painter.galley(pos2(rect.min.x + 24.0, y + 8.0), footer, muted_color());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReconnectStatus;

    #[test]
    fn ready_without_view_still_reports_synchronizing() {
        let state = StudioUiState::new(StudioConnectionStatus::Ready);
        assert_eq!(
            connection_card(Locale::ENGLISH, &state).unwrap().title,
            "Synchronizing"
        );
    }

    #[test]
    fn reconnecting_reports_attempt_and_delay() {
        let mut state = StudioUiState::new(StudioConnectionStatus::Backoff);
        assert_eq!(
            connection_card(Locale::ENGLISH, &state).unwrap().detail,
            "Waiting before the next connection attempt…"
        );
        state.reconnect = Some(ReconnectStatus {
            attempt: 3,
            delay_ms: 2_450,
        });
        let card = connection_card(Locale::ENGLISH, &state).unwrap();
        assert_eq!(card.title, "Reconnecting");
        assert_eq!(card.tone, CardTone::Busy);
        assert_eq!(
            card.detail,
            "Connection lost · attempt 3 · retrying after 2.4 s"
        );
        assert_eq!(card.last_error, None);
        state.error = Some("TCP connect refused".to_owned());
        assert_eq!(
            connection_card(Locale::ENGLISH, &state)
                .unwrap()
                .last_error
                .as_deref(),
            Some("TCP connect refused")
        );
    }

    #[test]
    fn terminal_states_use_error_tone() {
        let state = StudioUiState::new(StudioConnectionStatus::ProtocolMismatch);
        let card = connection_card(Locale::ENGLISH, &state).unwrap();
        assert_eq!(card.title, "Protocol mismatch");
        assert_eq!(card.tone, CardTone::Error);
        let mut state = StudioUiState::new(StudioConnectionStatus::Failed);
        state.error = Some("daemon exited".to_owned());
        let failed = connection_card(Locale::ENGLISH, &state).unwrap();
        assert_eq!(failed.detail, "daemon exited");
        // The failure detail already is the error; it is not repeated.
        assert_eq!(failed.last_error, None);
        let state = StudioUiState::new(StudioConnectionStatus::Disconnected);
        assert_eq!(
            connection_card(Locale::ENGLISH, &state).unwrap().tone,
            CardTone::Idle
        );
    }

    #[test]
    fn the_card_is_worded_in_the_console_language() {
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        let mut state = StudioUiState::new(StudioConnectionStatus::Backoff);
        state.reconnect = Some(ReconnectStatus {
            attempt: 2,
            delay_ms: 1_500,
        });
        let card = connection_card(spanish, &state).unwrap();
        assert_eq!(card.title, "Reconectando");
        assert_eq!(
            card.detail,
            "Conexión perdida · intento 2 · reintento en 1,5 s"
        );
    }
}
