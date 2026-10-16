//! Network stream inputs in the console (IN-015): the Add input dialog's
//! stream form, and the receive badge and details on an input's tile.
//!
//! The form validates as the operator types, with the same rules the daemon
//! applies, so Add is only enabled for a stream the daemon accepts. The
//! tile badge reads the input's latest `network_stream_status` (LIVE,
//! BUFFERING, STALLED, RECONNECTING n, FAILED); the tile's tooltip carries
//! the URL (redacted by the daemon), bitrate, buffer, dropped and late
//! frames, reconnects, and the last error.

use egui::{
    Color32, CornerRadius, DragValue, FontFamily, FontId, Painter, Rect, RichText, Ui, Vec2, pos2,
};
use fm_protocol::{
    GiveUp, NetworkStreamConfig, NetworkStreamEntry, NetworkStreamError, NetworkStreamSettings,
    NetworkStreamState, NetworkStreamStatus, RtspTransport, StallSlate, StreamProtocol,
    validate_stream_url,
};
use fm_types::network_stream::{
    MAX_JITTER_BUFFER_MS, MAX_RECONNECT_ATTEMPTS, MAX_RECONNECT_BACKOFF_MS, MAX_STREAM_TIMEOUT_MS,
    MAX_STREAM_URL_BYTES, MIN_JITTER_BUFFER_MS, MIN_RECONNECT_BACKOFF_MS, MIN_STREAM_TIMEOUT_MS,
};

use crate::input_catalogue::kind_key;
use crate::l10n::{Arg, Locale, Msg};
use crate::theme::{AMBER, ERROR, muted_color, section_label, text_dim_color};

/// The stream form of the Add input dialog.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct NetworkStreamDraft {
    pub(crate) url: String,
    pub(crate) settings: NetworkStreamSettings,
    /// Reconnect attempts before the input fails; zero never gives up.
    pub(crate) give_up_after: u32,
}

impl NetworkStreamDraft {
    /// The configuration the form describes, or why the daemon would refuse
    /// it (an English catalog reason).
    pub(crate) fn config(&self) -> Result<NetworkStreamConfig, &'static str> {
        let url = self.url.trim();
        if url.is_empty() {
            return Err(Msg::NsReasonUrlEmpty.en());
        }
        let mut settings = self.settings;
        settings.reconnect.give_up =
            core::num::NonZeroU32::new(self.give_up_after).map_or(GiveUp::Never, GiveUp::After);
        let config = NetworkStreamConfig {
            url: url.to_owned(),
            settings,
        };
        config.validate().map_err(|error| match error {
            NetworkStreamError::InvalidUrl => Msg::NsReasonUrlInvalid.en(),
            NetworkStreamError::UnsupportedScheme => Msg::RsnUnsupportedScheme.en(),
            NetworkStreamError::TransportNotRtsp => Msg::NsReasonTransport.en(),
            NetworkStreamError::Backoff => Msg::NsReasonBackoff.en(),
            NetworkStreamError::JitterBuffer
            | NetworkStreamError::GiveUp
            | NetworkStreamError::Timeout => Msg::NsReasonSettings.en(),
        })?;
        Ok(config)
    }

    /// Whether the URL names an RTSP stream (the transport hint applies).
    fn is_rtsp(&self) -> bool {
        validate_stream_url(self.url.trim()) == Ok(StreamProtocol::Rtsp)
    }
}

/// Draws the stream form: URL, RTSP transport, jitter buffer, reconnect
/// policy, timeout, and stall slate, each with its help.
#[allow(clippy::too_many_lines)] // one labelled control per setting
pub(crate) fn draw_stream_form(ui: &mut Ui, l: Locale, draft: &mut NetworkStreamDraft) {
    ui.label(section_label(l.text(Msg::NsUrl)));
    ui.add(
        egui::TextEdit::singleline(&mut draft.url)
            .id(egui::Id::new("studio-add-input-stream-url"))
            .hint_text("rtsp://camera.local:554/stream1")
            .char_limit(MAX_STREAM_URL_BYTES)
            .desired_width(ui.available_width()),
    )
    .on_hover_text(l.text(Msg::NsUrlHover));
    if let Err(reason) = draft.config() {
        ui.label(
            RichText::new(l.tr(reason))
                .size(crate::theme::legible(10.5))
                .color(if draft.url.trim().is_empty() {
                    muted_color()
                } else {
                    ERROR
                }),
        );
    }
    ui.add_space(4.0);
    let rtsp = draft.is_rtsp();
    if !rtsp {
        draft.settings.transport = RtspTransport::Auto;
    }
    ui.label(section_label(l.text(Msg::NsTransport)));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (transport, label) in [
            (RtspTransport::Auto, Msg::NsTransportAuto),
            (RtspTransport::Tcp, Msg::NsTransportTcp),
            (RtspTransport::Udp, Msg::NsTransportUdp),
        ] {
            let selected = draft.settings.transport == transport;
            let response = ui
                .add_enabled(rtsp, kind_key(l.text(label), selected))
                .on_hover_text(l.text(Msg::NsTransportHover))
                .on_disabled_hover_text(format!(
                    "{}\n{}",
                    l.text(Msg::NsTransportHover),
                    crate::l10n::unavailable(l, Msg::NsReasonTransport.en())
                ));
            if response.clicked() {
                draft.settings.transport = transport;
            }
        }
    });
    ui.add_space(4.0);
    egui::Grid::new("studio-add-input-stream-grid")
        .num_columns(2)
        .spacing(Vec2::new(10.0, 4.0))
        .show(ui, |ui| {
            number_row(
                ui,
                l,
                (Msg::NsJitter, Msg::NsJitterHover),
                &mut draft.settings.jitter_buffer_ms,
                MIN_JITTER_BUFFER_MS..=MAX_JITTER_BUFFER_MS,
            );
            number_row(
                ui,
                l,
                (Msg::NsBackoffMin, Msg::NsBackoffMinHover),
                &mut draft.settings.reconnect.backoff_min_ms,
                MIN_RECONNECT_BACKOFF_MS..=MAX_RECONNECT_BACKOFF_MS,
            );
            number_row(
                ui,
                l,
                (Msg::NsBackoffMax, Msg::NsBackoffMaxHover),
                &mut draft.settings.reconnect.backoff_max_ms,
                MIN_RECONNECT_BACKOFF_MS..=MAX_RECONNECT_BACKOFF_MS,
            );
            number_row(
                ui,
                l,
                (Msg::NsGiveUp, Msg::NsGiveUpHover),
                &mut draft.give_up_after,
                0..=MAX_RECONNECT_ATTEMPTS,
            );
            number_row(
                ui,
                l,
                (Msg::NsTimeout, Msg::NsTimeoutHover),
                &mut draft.settings.timeout_ms,
                MIN_STREAM_TIMEOUT_MS..=MAX_STREAM_TIMEOUT_MS,
            );
        });
    ui.add_space(4.0);
    ui.label(section_label(l.text(Msg::NsStall)));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (stall, label, hover) in [
            (
                StallSlate::Freeze,
                Msg::NsStallFreeze,
                Msg::NsStallFreezeHover,
            ),
            (StallSlate::Black, Msg::FtbWordBlack, Msg::NsStallBlackHover),
        ] {
            let selected = draft.settings.stall == stall;
            if ui
                .add(kind_key(l.text(label), selected))
                .on_hover_text(l.text(hover))
                .clicked()
            {
                draft.settings.stall = stall;
            }
        }
    });
}

fn number_row(
    ui: &mut Ui,
    l: Locale,
    (label, hover): (Msg, Msg),
    value: &mut u32,
    range: core::ops::RangeInclusive<u32>,
) {
    ui.label(
        RichText::new(l.text(label))
            .size(crate::theme::legible(11.0))
            .color(text_dim_color()),
    )
    .on_hover_text(l.text(hover));
    ui.add(DragValue::new(value).range(range).speed(10.0))
        .on_hover_text(l.text(hover));
    ui.end_row();
}

/// The tile badge for a stream status: its text and colour, or `None`
/// while nothing receives the stream (idle).
pub(crate) fn stream_badge(l: Locale, status: &NetworkStreamStatus) -> Option<(String, Color32)> {
    Some(match status.state {
        NetworkStreamState::Idle => return None,
        NetworkStreamState::Connecting => (l.text(Msg::ConnConnecting).to_owned(), muted_color()),
        NetworkStreamState::Buffering => (l.text(Msg::NsBadgeBuffering).to_owned(), AMBER),
        NetworkStreamState::Live => (l.text(Msg::Live).to_owned(), Color32::from_rgb(30, 150, 70)),
        NetworkStreamState::Stalled => (l.text(Msg::NsBadgeStalled).to_owned(), AMBER),
        NetworkStreamState::Reconnecting { attempt, .. } => (
            l.format(
                Msg::NsBadgeReconnecting,
                &[("attempt", Arg::from(u64::from(attempt)))],
            ),
            AMBER,
        ),
        NetworkStreamState::Failed => (l.text(Msg::ConnFailed).to_owned(), ERROR),
    })
}

/// The tile tooltip line for a network stream input: the tile help stays
/// within its four lines, so the details share one.
pub(crate) fn stream_details(l: Locale, entry: &NetworkStreamEntry) -> String {
    let status = &entry.status;
    let state = stream_badge(l, status)
        .map_or_else(|| l.text(Msg::NsBadgeIdle).to_owned(), |(text, _)| text);
    let mut lines = vec![
        l.format(
            Msg::NsDetailStream,
            &[
                ("url", Arg::Str(&entry.config.url)),
                ("state", Arg::Str(&state)),
            ],
        ),
        l.format(
            Msg::NsDetailRates,
            &[
                ("kbps", Arg::from(status.bitrate_bps / 1_000)),
                ("buffer", Arg::from(u64::from(status.buffer_ms))),
            ],
        ),
        l.format(
            Msg::NsDetailCounters,
            &[
                ("dropped", Arg::from(status.dropped_frames)),
                ("late", Arg::from(status.late_frames)),
                ("reconnects", Arg::from(u64::from(status.reconnects))),
            ],
        ),
    ];
    if let NetworkStreamState::Reconnecting { next_in_ms, .. } = status.state {
        lines.push(l.format(
            Msg::NsDetailNextAttempt,
            &[("seconds", Arg::from(u64::from(next_in_ms.div_ceil(1_000))))],
        ));
    }
    if let Some(error) = &status.last_error {
        lines.push(l.format(Msg::CardLastError, &[("error", Arg::Str(error))]));
    }
    lines.join(" · ")
}

/// Paints the stream badge in the picture's bottom trailing corner.
pub(crate) fn paint_stream_badge(
    painter: &Painter,
    thumb: Rect,
    l: Locale,
    status: &NetworkStreamStatus,
) {
    let Some((text, color)) = stream_badge(l, status) else {
        return;
    };
    let galley = painter.layout_no_wrap(
        text,
        FontId::new(9.0, FontFamily::Proportional),
        Color32::WHITE,
    );
    let badge = Rect::from_min_size(
        pos2(
            thumb.max.x - galley.size().x - 16.0,
            thumb.max.y - 15.0 - 8.0,
        ),
        Vec2::new(galley.size().x + 10.0, 15.0),
    );
    painter.rect_filled(badge, CornerRadius::same(3), color);
    painter.galley(badge.center() - galley.size() / 2.0, galley, Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use fm_ui_model::i18n::Language;

    use super::*;

    #[test]
    fn the_form_validates_like_the_daemon() {
        let mut draft = NetworkStreamDraft::default();
        assert_eq!(draft.config(), Err(Msg::NsReasonUrlEmpty.en()));
        for url in ["ftp://host/live", "file:///etc/passwd"] {
            draft.url = url.into();
            assert_eq!(draft.config(), Err(Msg::RsnUnsupportedScheme.en()), "{url}");
            assert_eq!(
                Locale::new(Language::Spanish).tr(draft.config().unwrap_err()),
                "esquema no admitido (admitidos: rtsp, rtp, udp, tcp, http, https, hls)"
            );
        }
        draft.url = "rtsp://".into();
        assert_eq!(draft.config(), Err(Msg::NsReasonUrlInvalid.en()));
        draft.url = " rtsp://cam.local/live ".into();
        draft.settings.transport = RtspTransport::Tcp;
        draft.give_up_after = 5;
        let config = draft.config().unwrap();
        assert_eq!(config.url, "rtsp://cam.local/live");
        assert_eq!(
            config.settings.reconnect.give_up,
            GiveUp::After(core::num::NonZeroU32::new(5).unwrap())
        );
        draft.url = "udp://@:9000".into();
        assert_eq!(draft.config(), Err(Msg::NsReasonTransport.en()));
        draft.settings.transport = RtspTransport::Auto;
        draft.settings.reconnect.backoff_min_ms = 20_000;
        draft.settings.reconnect.backoff_max_ms = 1_000;
        assert_eq!(draft.config(), Err(Msg::NsReasonBackoff.en()));
    }

    #[test]
    fn badges_name_each_receiving_state_in_both_languages() {
        let spanish = Locale::new(Language::Spanish);
        let status = |state| NetworkStreamStatus {
            state,
            ..NetworkStreamStatus::default()
        };
        assert_eq!(
            stream_badge(Locale::ENGLISH, &status(NetworkStreamState::Idle)),
            None
        );
        assert_eq!(
            stream_badge(Locale::ENGLISH, &status(NetworkStreamState::Live))
                .unwrap()
                .0,
            "LIVE"
        );
        let reconnecting = status(NetworkStreamState::Reconnecting {
            attempt: 3,
            next_in_ms: 1_200,
        });
        assert_eq!(
            stream_badge(Locale::ENGLISH, &reconnecting).unwrap().0,
            "RECONNECTING 3"
        );
        assert_eq!(
            stream_badge(spanish, &reconnecting).unwrap().0,
            "RECONECTANDO 3"
        );
        let details = stream_details(
            Locale::ENGLISH,
            &NetworkStreamEntry {
                input: fm_protocol::WireInputId::new(core::num::NonZeroU128::new(2).unwrap()),
                config: NetworkStreamConfig::new("rtsp://***@cam.local/live"),
                status: NetworkStreamStatus {
                    last_error: Some("Connection refused".into()),
                    ..reconnecting
                },
            },
        );
        assert!(details.contains("rtsp://***@cam.local/live"), "{details}");
        assert!(details.contains("Next attempt in 2 s"), "{details}");
        assert!(
            details.contains("Last error: Connection refused"),
            "{details}"
        );
        assert!(!details.contains('\n'), "{details}");
    }
}
