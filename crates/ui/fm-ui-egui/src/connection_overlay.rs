//! Centered status card over the monitors whenever controls are not Ready.

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Rect, Stroke, StrokeKind, Ui, Vec2, pos2,
};

use crate::theme::{AMBER, BORDER_STRONG, ERROR, GRAPHITE_RAISED, MUTED, TEXT, TEXT_DIM};
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

/// What every card states about the console's controls.
pub(crate) const CONTROLS_DISABLED: &str =
    "Switching, audio, and output controls are disabled until the console is ready.";

/// Formats a millisecond delay as seconds with one decimal.
fn format_delay(delay_ms: u64) -> String {
    format!("{}.{} s", delay_ms / 1_000, (delay_ms % 1_000) / 100)
}

/// Returns the card to show, or `None` when the console is Ready with state.
pub(crate) fn connection_card(state: &StudioUiState) -> Option<ConnectionCard> {
    // A terminal failure already shows its error as the detail.
    let last_error = match state.connection_status {
        StudioConnectionStatus::Failed => None,
        _ => state.error.clone(),
    };
    let card = |title: &str, detail: String, tone| {
        Some(ConnectionCard {
            title: title.to_owned(),
            detail,
            tone,
            last_error: last_error.clone(),
        })
    };
    match state.connection_status {
        StudioConnectionStatus::Ready if state.view.is_some() => None,
        StudioConnectionStatus::Ready | StudioConnectionStatus::Synchronizing => card(
            "Synchronizing",
            "Receiving the current project state from freemixd…".to_owned(),
            CardTone::Busy,
        ),
        StudioConnectionStatus::Launching => card(
            "Starting",
            "Launching the Studio control client…".to_owned(),
            CardTone::Busy,
        ),
        StudioConnectionStatus::Connecting => card(
            "Connecting",
            "Opening the control connection to freemixd…".to_owned(),
            CardTone::Busy,
        ),
        StudioConnectionStatus::Backoff => card(
            "Reconnecting",
            state.reconnect.map_or_else(
                || "Waiting before the next connection attempt…".to_owned(),
                |reconnect| {
                    format!(
                        "Connection lost · attempt {} · retrying after {}",
                        reconnect.attempt,
                        format_delay(reconnect.delay_ms)
                    )
                },
            ),
            CardTone::Busy,
        ),
        StudioConnectionStatus::Disconnected => card(
            "Disconnected",
            "There is no control connection to freemixd. Controls stay disabled until it reconnects."
                .to_owned(),
            CardTone::Idle,
        ),
        StudioConnectionStatus::Failed => card(
            "Connection failed",
            state.error.clone().unwrap_or_else(|| {
                "The control connection failed. Check that freemixd is running.".to_owned()
            }),
            CardTone::Error,
        ),
        StudioConnectionStatus::ProtocolMismatch => card(
            "Protocol mismatch",
            "freemixd did not select this Studio's protocol version. Run matching freemixd and freemix-studio builds."
                .to_owned(),
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
        CardTone::Idle => MUTED,
    };
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("studio-connection-card"),
    ));
    let painter = painter.with_clip_rect(area);
    let width = area.width().clamp(240.0, 480.0);
    let detail = painter.layout(
        card.detail.clone(),
        FontId::new(12.5, FontFamily::Proportional),
        TEXT_DIM,
        width - 48.0,
    );
    let error = card.last_error.as_ref().map(|error| {
        let mut job = egui::text::LayoutJob::simple(
            format!("Last error: {error}"),
            FontId::new(11.0, FontFamily::Monospace),
            ERROR,
            width - 48.0,
        );
        job.wrap.max_rows = 3;
        painter.layout_job(job)
    });
    let footer = painter.layout(
        CONTROLS_DISABLED.to_owned(),
        FontId::new(11.0, FontFamily::Proportional),
        MUTED,
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
    painter.rect_filled(rect, CornerRadius::same(8), GRAPHITE_RAISED);
    painter.rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, BORDER_STRONG),
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
    painter.text(
        pos2(rect.min.x + 44.0, title_y),
        Align2::LEFT_CENTER,
        &card.title,
        FontId::new(18.0, FontFamily::Proportional),
        TEXT,
    );
    let mut y = title_y + 22.0;
    let detail_height = detail.size().y;
    painter.galley(pos2(rect.min.x + 24.0, y), detail, TEXT_DIM);
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
        Stroke::new(1.0, BORDER_STRONG.gamma_multiply(0.6)),
    );
    painter.galley(pos2(rect.min.x + 24.0, y + 8.0), footer, MUTED);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReconnectStatus;

    #[test]
    fn ready_without_view_still_reports_synchronizing() {
        let state = StudioUiState::new(StudioConnectionStatus::Ready);
        assert_eq!(connection_card(&state).unwrap().title, "Synchronizing");
    }

    #[test]
    fn reconnecting_reports_attempt_and_delay() {
        let mut state = StudioUiState::new(StudioConnectionStatus::Backoff);
        assert_eq!(
            connection_card(&state).unwrap().detail,
            "Waiting before the next connection attempt…"
        );
        state.reconnect = Some(ReconnectStatus {
            attempt: 3,
            delay_ms: 2_450,
        });
        let card = connection_card(&state).unwrap();
        assert_eq!(card.title, "Reconnecting");
        assert_eq!(card.tone, CardTone::Busy);
        assert_eq!(
            card.detail,
            "Connection lost · attempt 3 · retrying after 2.4 s"
        );
        assert_eq!(card.last_error, None);
        state.error = Some("TCP connect refused".to_owned());
        assert_eq!(
            connection_card(&state).unwrap().last_error.as_deref(),
            Some("TCP connect refused")
        );
    }

    #[test]
    fn terminal_states_use_error_tone() {
        let state = StudioUiState::new(StudioConnectionStatus::ProtocolMismatch);
        let card = connection_card(&state).unwrap();
        assert_eq!(card.title, "Protocol mismatch");
        assert_eq!(card.tone, CardTone::Error);
        let mut state = StudioUiState::new(StudioConnectionStatus::Failed);
        state.error = Some("daemon exited".to_owned());
        let failed = connection_card(&state).unwrap();
        assert_eq!(failed.detail, "daemon exited");
        // The failure detail already is the error; it is not repeated.
        assert_eq!(failed.last_error, None);
        let state = StudioUiState::new(StudioConnectionStatus::Disconnected);
        assert_eq!(connection_card(&state).unwrap().tone, CardTone::Idle);
    }
}
