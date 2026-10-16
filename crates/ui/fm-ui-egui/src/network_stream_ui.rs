//! Network stream inputs in the console (IN-015, IN-016, IN-017): the Add
//! input dialog's stream form, and the receive badge and details on an
//! input's tile.
//!
//! The form validates as the operator types, with the same rules the daemon
//! applies, so Add is only enabled for a stream the daemon accepts. An
//! `rtmp://` URL adds the connection mode (pull from a server, or listen
//! for an encoder's push) and the stream key; an `srt://` URL adds the mode
//! (caller, listener, or rendezvous), passphrase and key length, stream
//! ID, and latency. Every stream may hear one stereo pair of a
//! multichannel source. The tile badge reads the input's latest
//! `network_stream_status` (LISTENING, BUFFERING, LIVE, STALLED,
//! RECONNECTING n, FAILED); the tile's tooltip carries the URL (redacted by
//! the daemon), the RTMP or SRT connection (mode, latency, encryption),
//! bitrate, buffer, dropped and late frames, reconnects, the source's audio
//! channels, and the last error.

use egui::{
    Color32, CornerRadius, DragValue, FontFamily, FontId, Painter, Rect, RichText, Ui, Vec2, pos2,
};
use fm_protocol::{
    AudioChannels, GiveUp, NetworkStreamConfig, NetworkStreamEntry, NetworkStreamError,
    NetworkStreamSettings, NetworkStreamState, NetworkStreamStatus, RtspTransport, SrtKeyLength,
    StallSlate, StreamMode, StreamProtocol, validate_stream_url,
};
use fm_types::network_stream::{
    MAX_JITTER_BUFFER_MS, MAX_RECONNECT_ATTEMPTS, MAX_RECONNECT_BACKOFF_MS, MAX_SRT_LATENCY_MS,
    MAX_SRT_PASSPHRASE_BYTES, MAX_SRT_STREAM_ID_BYTES, MAX_STREAM_KEY_BYTES, MAX_STREAM_TIMEOUT_MS,
    MAX_STREAM_URL_BYTES, MIN_JITTER_BUFFER_MS, MIN_RECONNECT_BACKOFF_MS, MIN_SRT_LATENCY_MS,
    MIN_STREAM_TIMEOUT_MS,
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
    /// RTMP stream key; empty for none.
    pub(crate) stream_key: String,
    /// SRT passphrase; empty for no encryption.
    pub(crate) passphrase: String,
    /// SRT stream ID; empty for none.
    pub(crate) stream_id: String,
}

impl NetworkStreamDraft {
    /// The configuration the form describes, or why the daemon would refuse
    /// it (an English catalog reason). Only the fields of the URL's
    /// protocol are kept: a stream key typed for an `rtmp://` URL is not
    /// sent once the URL becomes `srt://`.
    pub(crate) fn config(&self) -> Result<NetworkStreamConfig, &'static str> {
        let url = self.url.trim();
        if url.is_empty() {
            return Err(Msg::NsReasonUrlEmpty.en());
        }
        let protocol = self.protocol();
        let mut settings = self.settings;
        settings.reconnect.give_up =
            core::num::NonZeroU32::new(self.give_up_after).map_or(GiveUp::Never, GiveUp::After);
        let field = |wanted: StreamProtocol, value: &str| {
            (protocol == Some(wanted) && !value.is_empty()).then(|| value.to_owned())
        };
        let config = NetworkStreamConfig {
            url: url.to_owned(),
            settings,
            stream_id: field(StreamProtocol::Srt, &self.stream_id),
            stream_key: field(StreamProtocol::Rtmp, &self.stream_key),
            passphrase: field(StreamProtocol::Srt, &self.passphrase),
        };
        config.validate().map_err(|error| match error {
            NetworkStreamError::InvalidUrl => Msg::NsReasonUrlInvalid.en(),
            NetworkStreamError::UnsupportedScheme => Msg::RsnUnsupportedScheme.en(),
            NetworkStreamError::TransportNotRtsp => Msg::NsReasonTransport.en(),
            NetworkStreamError::Backoff => Msg::NsReasonBackoff.en(),
            NetworkStreamError::JitterBuffer
            | NetworkStreamError::GiveUp
            | NetworkStreamError::Timeout
            | NetworkStreamError::Latency => Msg::NsReasonSettings.en(),
            NetworkStreamError::ModeUnsupported => Msg::NsReasonMode.en(),
            NetworkStreamError::ListenPort => Msg::NsReasonListenPort.en(),
            NetworkStreamError::StreamKey => Msg::NsReasonStreamKey.en(),
            NetworkStreamError::Passphrase => Msg::NsReasonPassphrase.en(),
            NetworkStreamError::StreamId => Msg::NsReasonStreamId.en(),
        })?;
        Ok(config)
    }

    /// The protocol the URL names, if it is a valid stream URL.
    fn protocol(&self) -> Option<StreamProtocol> {
        validate_stream_url(self.url.trim()).ok()
    }
}

/// Draws the stream form: URL, the RTMP or SRT connection fields, RTSP
/// transport, audio channels, jitter buffer, reconnect policy, timeout, and
/// stall slate, each with its help.
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
    let protocol = draft.protocol();
    if let Some(protocol) = protocol
        && !draft.settings.mode.supported_by(protocol)
    {
        draft.settings.mode = StreamMode::Caller;
    }
    if matches!(protocol, Some(StreamProtocol::Rtmp | StreamProtocol::Srt)) {
        draw_connection_fields(ui, l, draft, protocol == Some(StreamProtocol::Srt));
        ui.add_space(4.0);
    }
    let rtsp = protocol == Some(StreamProtocol::Rtsp);
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
    ui.label(section_label(l.text(Msg::NsAudio)));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for audio in AudioChannels::ALL {
            let selected = draft.settings.audio == audio;
            let (label, hover) = match audio.first_channel() {
                None => (
                    l.text(Msg::NsAudioMix).to_owned(),
                    l.text(Msg::NsAudioMixHover).to_owned(),
                ),
                Some(left) => (
                    audio.token().to_owned(),
                    l.format(
                        Msg::NsAudioPairHover,
                        &[
                            ("left", Arg::from(u64::from(left) + 1)),
                            ("right", Arg::from(u64::from(left) + 2)),
                        ],
                    ),
                ),
            };
            if ui
                .add(kind_key(&label, selected))
                .on_hover_text(hover)
                .clicked()
            {
                draft.settings.audio = audio;
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

/// The RTMP (`srt == false`) or SRT connection fields: the mode, then the
/// stream key, or the passphrase, key length, stream ID, and latency.
fn draw_connection_fields(ui: &mut Ui, l: Locale, draft: &mut NetworkStreamDraft, srt: bool) {
    ui.label(section_label(l.text(Msg::NsMode)));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (mode, label, hover) in [
            (
                StreamMode::Caller,
                Msg::NsModeCaller,
                Msg::NsModeCallerHover,
            ),
            (
                StreamMode::Listener,
                Msg::NsModeListener,
                Msg::NsModeListenerHover,
            ),
            (
                StreamMode::Rendezvous,
                Msg::NsModeRendezvous,
                Msg::NsModeRendezvousHover,
            ),
        ] {
            let enabled = srt || mode != StreamMode::Rendezvous;
            let selected = draft.settings.mode == mode;
            let response = ui
                .add_enabled(enabled, kind_key(l.text(label), selected))
                .on_hover_text(l.text(hover))
                .on_disabled_hover_text(format!(
                    "{}\n{}",
                    l.text(hover),
                    crate::l10n::unavailable(l, Msg::NsReasonMode.en())
                ));
            if response.clicked() {
                draft.settings.mode = mode;
            }
        }
    });
    let secret_row = |ui: &mut Ui, label: Msg, hover: Msg, value: &mut String, limit: usize| {
        ui.label(
            RichText::new(l.text(label))
                .size(crate::theme::legible(11.0))
                .color(text_dim_color()),
        )
        .on_hover_text(l.text(hover));
        ui.add(
            egui::TextEdit::singleline(value)
                .password(true)
                .char_limit(limit)
                .desired_width(220.0),
        )
        .on_hover_text(l.text(hover));
        ui.end_row();
    };
    egui::Grid::new("studio-add-input-stream-connection")
        .num_columns(2)
        .spacing(Vec2::new(10.0, 4.0))
        .show(ui, |ui| {
            if !srt {
                secret_row(
                    ui,
                    Msg::NsStreamKey,
                    Msg::NsStreamKeyHover,
                    &mut draft.stream_key,
                    MAX_STREAM_KEY_BYTES,
                );
                return;
            }
            secret_row(
                ui,
                Msg::NsPassphrase,
                Msg::NsPassphraseHover,
                &mut draft.passphrase,
                MAX_SRT_PASSPHRASE_BYTES,
            );
            ui.label(
                RichText::new(l.text(Msg::NsStreamId))
                    .size(crate::theme::legible(11.0))
                    .color(text_dim_color()),
            )
            .on_hover_text(l.text(Msg::NsStreamIdHover));
            ui.add(
                egui::TextEdit::singleline(&mut draft.stream_id)
                    .char_limit(MAX_SRT_STREAM_ID_BYTES)
                    .desired_width(220.0),
            )
            .on_hover_text(l.text(Msg::NsStreamIdHover));
            ui.end_row();
            number_row(
                ui,
                l,
                (Msg::NsLatency, Msg::NsLatencyHover),
                &mut draft.settings.latency_ms,
                MIN_SRT_LATENCY_MS..=MAX_SRT_LATENCY_MS,
            );
        });
    if srt {
        draw_key_length(ui, l, draft);
    }
}

/// The SRT AES key length keys, enabled once a passphrase is typed.
fn draw_key_length(ui: &mut Ui, l: Locale, draft: &mut NetworkStreamDraft) {
    ui.label(section_label(l.text(Msg::NsKeyLength)));
    let encrypted = !draft.passphrase.is_empty();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for length in SrtKeyLength::ALL {
            let label = format!("AES-{}", u32::from(length.bytes()) * 8);
            let selected = draft.settings.key_length == length;
            let response = ui
                .add_enabled(encrypted, kind_key(&label, selected))
                .on_hover_text(l.text(Msg::NsKeyLengthHover))
                .on_disabled_hover_text(format!(
                    "{}\n{}",
                    l.text(Msg::NsKeyLengthHover),
                    crate::l10n::unavailable(l, Msg::NsReasonKeyLength.en())
                ));
            if response.clicked() {
                draft.settings.key_length = length;
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
        NetworkStreamState::Listening => (l.text(Msg::NsBadgeListening).to_owned(), muted_color()),
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
    if let Some(connection) = connection_details(l, &entry.config) {
        lines.insert(1, connection);
    }
    if status.source_channels > 0 {
        let hearing = match entry.config.settings.audio.first_channel() {
            None => l.text(Msg::NsAudioMix).to_owned(),
            Some(_) => entry.config.settings.audio.token().to_owned(),
        };
        lines.push(l.format(
            Msg::NsDetailAudio,
            &[
                ("count", Arg::from(u64::from(status.source_channels))),
                ("pair", Arg::Str(&hearing)),
            ],
        ));
    }
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

/// The RTMP or SRT connection line of the tile tooltip: the mode, and for
/// SRT the latency and encryption; `None` for other protocols.
fn connection_details(l: Locale, config: &NetworkStreamConfig) -> Option<String> {
    let protocol = validate_stream_url(&config.url).ok()?;
    let settings = &config.settings;
    let mode = l.text(match settings.mode {
        StreamMode::Caller => Msg::NsModeCaller,
        StreamMode::Listener => Msg::NsModeListener,
        StreamMode::Rendezvous => Msg::NsModeRendezvous,
    });
    match protocol {
        StreamProtocol::Rtmp => Some(l.format(
            Msg::NsDetailRtmp,
            &[
                ("mode", Arg::Str(mode)),
                (
                    "key",
                    Arg::Str(l.text(if config.stream_key.is_some() {
                        Msg::NsDetailKeySet
                    } else {
                        Msg::NsDetailKeyNone
                    })),
                ),
            ],
        )),
        StreamProtocol::Srt => {
            let encryption = if config.passphrase.is_some() {
                format!("AES-{}", u32::from(settings.key_length.bytes()) * 8)
            } else {
                l.text(Msg::NsDetailUnencrypted).to_owned()
            };
            Some(l.format(
                Msg::NsDetailSrt,
                &[
                    ("mode", Arg::Str(mode)),
                    ("latency", Arg::from(u64::from(settings.latency_ms))),
                    ("encryption", Arg::Str(&encryption)),
                ],
            ))
        }
        _ => None,
    }
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
                "esquema no admitido (admitidos: rtsp, rtp, udp, tcp, http, https, hls, rtmp, \
                 rtmps, srt)"
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

    #[test]
    fn the_form_keeps_only_the_fields_of_its_protocol() {
        let mut draft = NetworkStreamDraft {
            url: "rtmp://0.0.0.0:1935/live".into(),
            stream_key: "sk_live".into(),
            passphrase: "0123456789".into(),
            stream_id: "cam".into(),
            ..NetworkStreamDraft::default()
        };
        draft.settings.mode = StreamMode::Listener;
        let config = draft.config().unwrap();
        assert_eq!(config.stream_key.as_deref(), Some("sk_live"));
        assert_eq!(config.passphrase, None);
        assert_eq!(config.stream_id, None);
        draft.url = "rtmp://0.0.0.0:80/live".into();
        assert_eq!(draft.config(), Err(Msg::NsReasonListenPort.en()));
        draft.url = "srt://0.0.0.0:9000".into();
        let config = draft.config().unwrap();
        assert_eq!(config.stream_key, None);
        assert_eq!(config.passphrase.as_deref(), Some("0123456789"));
        assert_eq!(config.stream_id.as_deref(), Some("cam"));
        draft.passphrase = "short".into();
        assert_eq!(draft.config(), Err(Msg::NsReasonPassphrase.en()));
        draft.passphrase.clear();
        draft.settings.mode = StreamMode::Rendezvous;
        draft.url = "rtmp://h/live".into();
        assert_eq!(draft.config(), Err(Msg::NsReasonMode.en()));
    }

    #[test]
    fn srt_details_name_mode_latency_encryption_and_source_audio() {
        let spanish = Locale::new(Language::Spanish);
        let mut config = NetworkStreamConfig::new("srt://enc.test:9000");
        config.settings.mode = StreamMode::Listener;
        config.settings.latency_ms = 200;
        config.settings.key_length = SrtKeyLength::Aes256;
        config.settings.audio = AudioChannels::Pair34;
        config.passphrase = Some(fm_types::network_stream::REDACTED.into());
        let entry = NetworkStreamEntry {
            input: fm_protocol::WireInputId::new(core::num::NonZeroU128::new(2).unwrap()),
            config,
            status: NetworkStreamStatus {
                state: NetworkStreamState::Live,
                bitrate_bps: 4_000_000,
                source_channels: 8,
                ..NetworkStreamStatus::default()
            },
        };
        let details = stream_details(Locale::ENGLISH, &entry);
        assert!(
            details.contains("SRT LISTENER, 200 ms latency, AES-256"),
            "{details}"
        );
        assert!(details.contains("4,000 kb/s"), "{details}");
        assert!(
            details.contains("Source audio: 8 channels, hearing 3-4"),
            "{details}"
        );
        let details = stream_details(spanish, &entry);
        assert!(details.contains("SRT ESCUCHA"), "{details}");
        assert_eq!(
            stream_badge(
                Locale::ENGLISH,
                &NetworkStreamStatus {
                    state: NetworkStreamState::Listening,
                    ..NetworkStreamStatus::default()
                }
            )
            .unwrap()
            .0,
            "LISTENING"
        );
        let mut rtmp = NetworkStreamConfig::new("rtmp://0.0.0.0:1935/live");
        rtmp.stream_key = Some(fm_types::network_stream::REDACTED.into());
        assert_eq!(
            connection_details(Locale::ENGLISH, &rtmp).unwrap(),
            "RTMP CALLER, stream key set"
        );
    }
}
