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
    Color32, CornerRadius, DragValue, FontFamily, FontId, Painter, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2, pos2,
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

/// Which collapsed "Advanced" groups of the stream form are open.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct StreamAdvanced {
    /// Source: the audio channels.
    pub(crate) source: bool,
    /// Connection: the jitter buffer, SRT stream ID, and key length.
    pub(crate) connection: bool,
    /// Recovery: the retry waits and the timeout.
    pub(crate) recovery: bool,
}

/// Which secret fields show what was typed; every one is masked until its
/// eye key is pressed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct StreamReveal {
    /// The password in the URL's `user:password@` part.
    pub(crate) url: bool,
    pub(crate) stream_key: bool,
    pub(crate) passphrase: bool,
}

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
    pub(crate) advanced: StreamAdvanced,
    pub(crate) reveal: StreamReveal,
}

/// The stream form field a validation problem belongs to, so the problem
/// shows under that field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StreamField {
    Url,
    Transport,
    Mode,
    StreamKey,
    Passphrase,
    StreamId,
    Latency,
    Jitter,
    /// The first and longest retry waits, which are checked together.
    Backoff,
    GiveUp,
    Timeout,
}

impl StreamField {
    /// The field's label, as the form shows it.
    pub(crate) const fn label(self) -> Msg {
        match self {
            Self::Url => Msg::NsUrl,
            Self::Transport => Msg::NsTransport,
            Self::Mode => Msg::NsMode,
            Self::StreamKey => Msg::NsStreamKey,
            Self::Passphrase => Msg::NsPassphrase,
            Self::StreamId => Msg::NsStreamId,
            Self::Latency => Msg::NsLatency,
            Self::Jitter => Msg::NsJitter,
            Self::Backoff => Msg::NsBackoffMax,
            Self::GiveUp => Msg::NsGiveUp,
            Self::Timeout => Msg::NsTimeout,
        }
    }
}

/// Why the daemon would refuse a form, and the field to fix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FieldProblem<F> {
    pub(crate) field: F,
    /// An English catalog reason.
    pub(crate) reason: &'static str,
    /// The field is merely not filled in yet (no URL typed, no device
    /// chosen): a prompt, not an error, so it is not marked in red.
    pub(crate) prompt: bool,
}

impl<F> FieldProblem<F> {
    pub(crate) const fn error(field: F, reason: &'static str) -> Self {
        Self {
            field,
            reason,
            prompt: false,
        }
    }

    pub(crate) const fn prompt(field: F, reason: &'static str) -> Self {
        Self {
            field,
            reason,
            prompt: true,
        }
    }
}

impl NetworkStreamDraft {
    /// The configuration the form describes, or why the daemon would refuse
    /// it (an English catalog reason). Only the fields of the URL's
    /// protocol are kept: a stream key typed for an `rtmp://` URL is not
    /// sent once the URL becomes `srt://`.
    pub(crate) fn config(&self) -> Result<NetworkStreamConfig, &'static str> {
        self.checked().map_err(|problem| problem.reason)
    }

    /// [`Self::config`], with the field each problem belongs to.
    pub(crate) fn checked(&self) -> Result<NetworkStreamConfig, FieldProblem<StreamField>> {
        let url = self.url.trim();
        if url.is_empty() {
            return Err(FieldProblem::prompt(
                StreamField::Url,
                Msg::NsReasonUrlEmpty.en(),
            ));
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
        config.validate().map_err(|error| {
            let (field, reason) = match error {
                NetworkStreamError::InvalidUrl => (StreamField::Url, Msg::NsReasonUrlInvalid),
                NetworkStreamError::UnsupportedScheme => {
                    (StreamField::Url, Msg::RsnUnsupportedScheme)
                }
                NetworkStreamError::ListenPort => (StreamField::Url, Msg::NsReasonListenPort),
                NetworkStreamError::TransportNotRtsp => {
                    (StreamField::Transport, Msg::NsReasonTransport)
                }
                NetworkStreamError::Backoff => (StreamField::Backoff, Msg::NsReasonBackoff),
                NetworkStreamError::JitterBuffer => (StreamField::Jitter, Msg::NsReasonSettings),
                NetworkStreamError::GiveUp => (StreamField::GiveUp, Msg::NsReasonSettings),
                NetworkStreamError::Timeout => (StreamField::Timeout, Msg::NsReasonSettings),
                NetworkStreamError::Latency => (StreamField::Latency, Msg::NsReasonSettings),
                NetworkStreamError::ModeUnsupported => (StreamField::Mode, Msg::NsReasonMode),
                NetworkStreamError::StreamKey => (StreamField::StreamKey, Msg::NsReasonStreamKey),
                NetworkStreamError::Passphrase => {
                    (StreamField::Passphrase, Msg::NsReasonPassphrase)
                }
                NetworkStreamError::StreamId => (StreamField::StreamId, Msg::NsReasonStreamId),
            };
            FieldProblem::error(field, reason.en())
        })?;
        Ok(config)
    }

    /// The protocol the URL names, if it is a valid stream URL.
    fn protocol(&self) -> Option<StreamProtocol> {
        validate_stream_url(self.url.trim()).ok()
    }
}

/// The byte range of the password in a URL's `user:password@` part, if it
/// has a non-empty one.
pub(crate) fn url_password(url: &str) -> Option<core::ops::Range<usize>> {
    let start = url.find("://")? + 3;
    let rest = &url[start..];
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let at = authority.rfind('@')?;
    let colon = authority[..at].find(':')?;
    (colon + 1 < at).then(|| start + colon + 1..start + at)
}

/// `url` with the password of its `user:password@` part masked character
/// by character, so the text keeps its length and the cursor its place.
pub(crate) fn mask_url_password(url: &str) -> String {
    let Some(range) = url_password(url) else {
        return url.to_owned();
    };
    url.char_indices()
        .map(|(index, c)| {
            if range.contains(&index) {
                egui::epaint::text::PASSWORD_REPLACEMENT_CHAR
            } else {
                c
            }
        })
        .collect()
}

/// Height of one caption line under a field.
fn hint_height(ui: &Ui) -> f32 {
    ui.ctx().fonts_mut(|fonts| {
        fonts.row_height(&FontId::proportional(crate::theme::legible(
            crate::theme::CAPTION_SIZE,
        )))
    })
}

/// A hint line under a field that keeps its height when it is empty, so
/// the form does not shift as hints come and go; longer text wraps.
pub(crate) fn hint_slot(ui: &mut Ui, text: Option<RichText>) {
    let height = hint_height(ui);
    ui.vertical(|ui| {
        ui.set_min_height(height);
        if let Some(text) = text {
            ui.add(egui::Label::new(text).wrap());
        }
    });
}

/// A validation problem's text under the field it belongs to.
pub(crate) fn field_error(l: Locale, reason: &str) -> RichText {
    RichText::new(crate::input_catalogue::capitalized(l.tr(reason)))
        .size(crate::theme::legible(crate::theme::CAPTION_SIZE))
        .color(ERROR)
}

/// Outlines a field (or a row of keys) in red: it is the one to fix.
pub(crate) fn mark_invalid(ui: &Ui, rect: Rect) {
    ui.painter().rect_stroke(
        rect.expand(1.5),
        CornerRadius::same(4),
        Stroke::new(1.5, ERROR),
        StrokeKind::Outside,
    );
}

/// A form section's heading, with its Advanced disclosure (if any) at the
/// trailing end of the row.
pub(crate) fn section_heading(
    ui: &mut Ui,
    l: Locale,
    title: Msg,
    advanced: Option<(&mut bool, (Msg, Msg))>,
) {
    section_heading_with(ui, l, title, |ui| {
        if let Some((open, (show, hide))) = advanced {
            crate::theme::disclosure(
                ui,
                open,
                l.text(Msg::AdvancedKey),
                (l.text(show), l.text(hide)),
            );
        }
    });
}

/// A form section's heading with `trailing` controls at the end of its
/// row, over a rule.
pub(crate) fn section_heading_with(
    ui: &mut Ui,
    l: Locale,
    title: Msg,
    trailing: impl FnOnce(&mut Ui),
) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(section_label(l.text(title)).color(crate::theme::text_color()));
        ui.with_layout(crate::l10n::trailing_layout(l), trailing);
    });
    let rect = ui.available_rect_before_wrap();
    ui.painter().hline(
        rect.x_range(),
        rect.top(),
        Stroke::new(crate::theme::line_width(), crate::theme::border_color()),
    );
    ui.add_space(4.0);
}

/// A small field label above a control.
pub(crate) fn sub_label(ui: &mut Ui, l: Locale, label: Msg) {
    ui.label(
        RichText::new(l.text(label))
            .size(crate::theme::legible(11.0))
            .color(text_dim_color()),
    );
}

/// Paints an eye, struck through when `struck`, centred in `rect`.
#[allow(clippy::cast_precision_loss)] // a dozen segments
fn paint_eye(painter: &Painter, rect: Rect, color: Color32, struck: bool) {
    const SEGMENTS: usize = 12;
    let centre = rect.center();
    let (half_width, half_height) = (7.0_f32, 4.2_f32);
    let lid = |step: usize, sign: f32| {
        let x = -half_width + 2.0 * half_width * step as f32 / SEGMENTS as f32;
        centre + Vec2::new(x, sign * half_height * (1.0 - (x / half_width).powi(2)))
    };
    let mut points: Vec<_> = (0..=SEGMENTS).map(|step| lid(step, -1.0)).collect();
    points.extend((1..SEGMENTS).rev().map(|step| lid(step, 1.0)));
    painter.add(egui::Shape::closed_line(points, Stroke::new(1.3, color)));
    painter.circle_filled(centre, 2.1, color);
    if struck {
        painter.line_segment(
            [
                centre + Vec2::new(-half_width * 0.8, half_height + 1.5),
                centre + Vec2::new(half_width * 0.8, -half_height - 1.5),
            ],
            Stroke::new(1.6, color),
        );
    }
}

/// The show/hide key of a secret field: an eye while the field is masked
/// (a click shows it), a struck-through eye while it shows.
pub(crate) fn reveal_key(ui: &mut Ui, l: Locale, shown: &mut bool) -> egui::Response {
    let (name, hover) = if *shown {
        (Msg::SecretHide, Msg::SecretHideHover)
    } else {
        (Msg::SecretShow, Msg::SecretShowHover)
    };
    let response = ui
        .add(
            egui::Button::new("")
                .frame_when_inactive(false)
                .min_size(Vec2::splat(crate::theme::MIN_TARGET)),
        )
        .on_hover_text(l.text(hover));
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, l.text(name)));
    let color = ui.style().interact(&response).text_color();
    paint_eye(ui.painter(), response.rect, color, *shown);
    if response.clicked() {
        *shown = !*shown;
    }
    response
}

/// Width of a field that has an eye key beside it.
fn beside_key(ui: &Ui) -> f32 {
    (ui.available_width() - crate::theme::MIN_TARGET - ui.spacing().item_spacing.x).max(60.0)
}

/// Draws the stream form in three sections, each with its rarely changed
/// fields behind an Advanced disclosure: Source (URL; audio channels),
/// Connection (RTSP transport, or the RTMP / SRT mode, stream key or
/// passphrase, and SRT latency; jitter buffer, stream ID, key length), and
/// Recovery (stall slate and give-up count; retry waits and timeout). A
/// problem shows under the field it belongs to, which is outlined in red;
/// an Advanced group holding it opens.
pub(crate) fn draw_stream_form(ui: &mut Ui, l: Locale, draft: &mut NetworkStreamDraft) {
    let protocol = draft.protocol();
    if let Some(protocol) = protocol
        && !draft.settings.mode.supported_by(protocol)
    {
        draft.settings.mode = StreamMode::Caller;
    }
    if protocol != Some(StreamProtocol::Rtsp) {
        draft.settings.transport = RtspTransport::Auto;
    }
    let problem = draft.checked().err();
    let error = problem.filter(|problem| !problem.prompt);
    let at = |field: StreamField| error.filter(|problem| problem.field == field);
    match error.map(|problem| problem.field) {
        Some(StreamField::Jitter | StreamField::StreamId) => draft.advanced.connection = true,
        Some(StreamField::Backoff | StreamField::Timeout) => draft.advanced.recovery = true,
        _ => {}
    }
    draw_source(ui, l, draft, protocol, problem);
    draw_connection(ui, l, draft, protocol, &at);
    draw_recovery(ui, l, draft, &at);
}

type ProblemAt<'a> = dyn Fn(StreamField) -> Option<FieldProblem<StreamField>> + 'a;

/// The Source section: the URL (its password masked), and the audio
/// channels.
fn draw_source(
    ui: &mut Ui,
    l: Locale,
    draft: &mut NetworkStreamDraft,
    protocol: Option<StreamProtocol>,
    problem: Option<FieldProblem<StreamField>>,
) {
    section_heading(
        ui,
        l,
        Msg::NsSectionSource,
        Some((
            &mut draft.advanced.source,
            (Msg::NsAdvSourceShow, Msg::NsAdvSourceHide),
        )),
    );
    sub_label(ui, l, Msg::NsUrl);
    let has_password = url_password(&draft.url).is_some();
    ui.horizontal(|ui| {
        let reveal = draft.reveal.url;
        let mut layouter = move |ui: &Ui, text: &dyn egui::TextBuffer, _wrap: f32| {
            let shown = if reveal {
                text.as_str().to_owned()
            } else {
                mask_url_password(text.as_str())
            };
            let font = egui::FontSelection::Default.resolve(ui.style());
            let color = ui
                .visuals()
                .override_text_color
                .unwrap_or_else(|| ui.visuals().widgets.inactive.text_color());
            let mut job = egui::text::LayoutJob::simple_singleline(shown, font, color);
            job.keep_trailing_whitespace = true;
            ui.ctx().fonts_mut(|fonts| fonts.layout_job(job))
        };
        let width = beside_key(ui);
        let response = ui
            .add(
                egui::TextEdit::singleline(&mut draft.url)
                    .id(egui::Id::new("studio-add-input-stream-url"))
                    .hint_text("rtsp://camera.local:554/stream1")
                    .char_limit(MAX_STREAM_URL_BYTES)
                    .layouter(&mut layouter)
                    .desired_width(width),
            )
            .on_hover_text(l.text(Msg::NsUrlHover));
        if problem.is_some_and(|problem| !problem.prompt && problem.field == StreamField::Url) {
            mark_invalid(ui, response.rect);
        }
        // The key's place is kept while the URL has no password, so the
        // field does not change width as one is typed.
        if has_password {
            reveal_key(ui, l, &mut draft.reveal.url);
        } else {
            ui.allocate_exact_size(Vec2::splat(crate::theme::MIN_TARGET), Sense::hover());
        }
    });
    let hint = match problem {
        Some(problem) if problem.field == StreamField::Url => Some(if problem.prompt {
            crate::theme::caption(crate::input_catalogue::capitalized(l.tr(problem.reason)))
        } else {
            field_error(l, problem.reason)
        }),
        _ => protocol.map(|protocol| {
            crate::theme::caption(l.format(
                Msg::NsUrlRecognised,
                &[("protocol", Arg::Str(&protocol.token().to_uppercase()))],
            ))
        }),
    };
    hint_slot(ui, hint);
    if !draft.advanced.source {
        return;
    }
    sub_label(ui, l, Msg::NsAudio);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
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
                .add(kind_key(&label, selected).min_size(Vec2::new(64.0, 26.0)))
                .on_hover_text(hover)
                .clicked()
            {
                draft.settings.audio = audio;
            }
        }
    });
}

/// The Connection section: what the URL's protocol connects with.
fn draw_connection(
    ui: &mut Ui,
    l: Locale,
    draft: &mut NetworkStreamDraft,
    protocol: Option<StreamProtocol>,
    at: &ProblemAt<'_>,
) {
    section_heading(
        ui,
        l,
        Msg::NsSectionConnection,
        Some((
            &mut draft.advanced.connection,
            (Msg::NsAdvConnectionShow, Msg::NsAdvConnectionHide),
        )),
    );
    let srt = protocol == Some(StreamProtocol::Srt);
    let simple = matches!(
        protocol,
        Some(StreamProtocol::Rtsp | StreamProtocol::Rtmp | StreamProtocol::Srt)
    );
    match protocol {
        Some(StreamProtocol::Rtsp) => draw_transport(ui, l, draft, at),
        Some(StreamProtocol::Rtmp | StreamProtocol::Srt) => {
            draw_mode(ui, l, draft, srt, at);
            draw_connection_grid(ui, l, draft, srt, at);
        }
        _ => {}
    }
    if !draft.advanced.connection {
        if !simple {
            // Nothing else to set in the simple view, or nothing known yet.
            ui.label(crate::theme::caption(l.text(if protocol.is_some() {
                Msg::NsConnectionDefaults
            } else {
                Msg::NsConnectionPending
            })));
        }
        return;
    }
    egui::Grid::new("studio-add-input-stream-connection-advanced")
        .num_columns(2)
        .spacing(Vec2::new(10.0, 4.0))
        .show(ui, |ui| {
            number_row(
                ui,
                l,
                (Msg::NsJitter, Msg::NsJitterHover, None),
                &mut draft.settings.jitter_buffer_ms,
                MIN_JITTER_BUFFER_MS..=MAX_JITTER_BUFFER_MS,
                at(StreamField::Jitter),
            );
            if srt {
                ui.label(
                    RichText::new(l.text(Msg::NsStreamId))
                        .size(crate::theme::legible(11.0))
                        .color(text_dim_color()),
                )
                .on_hover_text(l.text(Msg::NsStreamIdHover));
                let response = ui
                    .add(
                        egui::TextEdit::singleline(&mut draft.stream_id)
                            .char_limit(MAX_SRT_STREAM_ID_BYTES)
                            .desired_width(220.0),
                    )
                    .on_hover_text(l.text(Msg::NsStreamIdHover));
                ui.end_row();
                if let Some(problem) = at(StreamField::StreamId) {
                    mark_invalid(ui, response.rect);
                    error_row(ui, l, problem.reason);
                }
            }
        });
    if srt {
        draw_key_length(ui, l, draft);
    }
}

/// The RTSP transport keys.
fn draw_transport(ui: &mut Ui, l: Locale, draft: &mut NetworkStreamDraft, at: &ProblemAt<'_>) {
    sub_label(ui, l, Msg::NsTransport);
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (transport, label) in [
            (RtspTransport::Auto, Msg::NsTransportAuto),
            (RtspTransport::Tcp, Msg::NsTransportTcp),
            (RtspTransport::Udp, Msg::NsTransportUdp),
        ] {
            let selected = draft.settings.transport == transport;
            if ui
                .add(kind_key(l.text(label), selected).min_size(Vec2::new(96.0, 26.0)))
                .on_hover_text(l.text(Msg::NsTransportHover))
                .clicked()
            {
                draft.settings.transport = transport;
            }
        }
    });
    if let Some(problem) = at(StreamField::Transport) {
        mark_invalid(ui, row.response.rect);
        ui.add(egui::Label::new(field_error(l, problem.reason)).wrap());
    }
}

/// The RTMP (`srt == false`) or SRT connection mode keys.
fn draw_mode(
    ui: &mut Ui,
    l: Locale,
    draft: &mut NetworkStreamDraft,
    srt: bool,
    at: &ProblemAt<'_>,
) {
    sub_label(ui, l, Msg::NsMode);
    let row = ui.horizontal(|ui| {
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
                .add_enabled(
                    enabled,
                    kind_key(l.text(label), selected).min_size(Vec2::new(120.0, 26.0)),
                )
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
    if let Some(problem) = at(StreamField::Mode) {
        mark_invalid(ui, row.response.rect);
        ui.add(egui::Label::new(field_error(l, problem.reason)).wrap());
    }
}

/// The RTMP stream key, or the SRT passphrase and latency; a secret is
/// masked until its eye key shows it.
fn draw_connection_grid(
    ui: &mut Ui,
    l: Locale,
    draft: &mut NetworkStreamDraft,
    srt: bool,
    at: &ProblemAt<'_>,
) {
    egui::Grid::new("studio-add-input-stream-connection")
        .num_columns(2)
        .spacing(Vec2::new(10.0, 4.0))
        .show(ui, |ui| {
            let (label, hover, value, limit, shown, field) = if srt {
                (
                    Msg::NsPassphrase,
                    Msg::NsPassphraseHover,
                    &mut draft.passphrase,
                    MAX_SRT_PASSPHRASE_BYTES,
                    &mut draft.reveal.passphrase,
                    StreamField::Passphrase,
                )
            } else {
                (
                    Msg::NsStreamKey,
                    Msg::NsStreamKeyHover,
                    &mut draft.stream_key,
                    MAX_STREAM_KEY_BYTES,
                    &mut draft.reveal.stream_key,
                    StreamField::StreamKey,
                )
            };
            ui.label(
                RichText::new(l.text(label))
                    .size(crate::theme::legible(11.0))
                    .color(text_dim_color()),
            )
            .on_hover_text(l.text(hover));
            ui.horizontal(|ui| {
                let response = ui
                    .add(
                        egui::TextEdit::singleline(value)
                            .password(!*shown)
                            .char_limit(limit)
                            .desired_width(220.0),
                    )
                    .on_hover_text(l.text(hover));
                if at(field).is_some() {
                    mark_invalid(ui, response.rect);
                }
                reveal_key(ui, l, shown);
            });
            ui.end_row();
            if let Some(problem) = at(field) {
                error_row(ui, l, problem.reason);
            }
            if srt {
                number_row(
                    ui,
                    l,
                    (Msg::NsLatency, Msg::NsLatencyHover, None),
                    &mut draft.settings.latency_ms,
                    MIN_SRT_LATENCY_MS..=MAX_SRT_LATENCY_MS,
                    at(StreamField::Latency),
                );
            }
        });
}

/// The Recovery section: what the input shows while the stream stalls, and
/// how it reconnects.
fn draw_recovery(ui: &mut Ui, l: Locale, draft: &mut NetworkStreamDraft, at: &ProblemAt<'_>) {
    section_heading(
        ui,
        l,
        Msg::NsSectionRecovery,
        Some((
            &mut draft.advanced.recovery,
            (Msg::NsAdvRecoveryShow, Msg::NsAdvRecoveryHide),
        )),
    );
    sub_label(ui, l, Msg::NsStall);
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
    ui.add_space(4.0);
    egui::Grid::new("studio-add-input-stream-recovery")
        .num_columns(2)
        .spacing(Vec2::new(10.0, 4.0))
        .show(ui, |ui| {
            number_row(
                ui,
                l,
                (Msg::NsGiveUp, Msg::NsGiveUpHover, Some(Msg::NsGiveUpNever)),
                &mut draft.give_up_after,
                0..=MAX_RECONNECT_ATTEMPTS,
                at(StreamField::GiveUp),
            );
            if !draft.advanced.recovery {
                return;
            }
            let backoff = at(StreamField::Backoff);
            number_row(
                ui,
                l,
                (Msg::NsBackoffMin, Msg::NsBackoffMinHover, None),
                &mut draft.settings.reconnect.backoff_min_ms,
                MIN_RECONNECT_BACKOFF_MS..=MAX_RECONNECT_BACKOFF_MS,
                // Both waits are outlined; the reason shows once, under the
                // pair.
                backoff.map(|problem| FieldProblem {
                    reason: "",
                    ..problem
                }),
            );
            number_row(
                ui,
                l,
                (Msg::NsBackoffMax, Msg::NsBackoffMaxHover, None),
                &mut draft.settings.reconnect.backoff_max_ms,
                MIN_RECONNECT_BACKOFF_MS..=MAX_RECONNECT_BACKOFF_MS,
                backoff,
            );
            number_row(
                ui,
                l,
                (Msg::NsTimeout, Msg::NsTimeoutHover, None),
                &mut draft.settings.timeout_ms,
                MIN_STREAM_TIMEOUT_MS..=MAX_STREAM_TIMEOUT_MS,
                at(StreamField::Timeout),
            );
        });
}

/// The SRT AES key length keys, enabled once a passphrase is typed.
fn draw_key_length(ui: &mut Ui, l: Locale, draft: &mut NetworkStreamDraft) {
    sub_label(ui, l, Msg::NsKeyLength);
    let encrypted = !draft.passphrase.is_empty();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for length in SrtKeyLength::ALL {
            let label = format!("AES-{}", u32::from(length.bytes()) * 8);
            let selected = draft.settings.key_length == length;
            let response = ui
                .add_enabled(
                    encrypted,
                    kind_key(&label, selected).min_size(Vec2::new(96.0, 26.0)),
                )
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

/// A grid row holding only a validation problem, under the control column.
fn error_row(ui: &mut Ui, l: Locale, reason: &str) {
    ui.label("");
    ui.add(egui::Label::new(field_error(l, reason)).wrap());
    ui.end_row();
}

/// A labelled number field; a `problem` outlines it in red and, unless its
/// reason is empty, shows that reason on the next row.
fn number_row(
    ui: &mut Ui,
    l: Locale,
    (label, hover, note): (Msg, Msg, Option<Msg>),
    value: &mut u32,
    range: core::ops::RangeInclusive<u32>,
    problem: Option<FieldProblem<StreamField>>,
) {
    ui.label(
        RichText::new(l.text(label))
            .size(crate::theme::legible(11.0))
            .color(text_dim_color()),
    )
    .on_hover_text(l.text(hover));
    let response = ui
        .horizontal(|ui| {
            let response = ui
                .add(DragValue::new(value).range(range).speed(10.0))
                .on_hover_text(l.text(hover));
            if let Some(note) = note {
                ui.label(crate::theme::caption(l.text(note)))
                    .on_hover_text(l.text(hover));
            }
            response
        })
        .inner;
    ui.end_row();
    if let Some(problem) = problem {
        mark_invalid(ui, response.rect);
        if !problem.reason.is_empty() {
            error_row(ui, l, problem.reason);
        }
    }
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
    paint_corner_badge(painter, thumb, text, color);
}

/// Paints a status badge in a picture's bottom trailing corner.
pub(crate) fn paint_corner_badge(painter: &Painter, thumb: Rect, text: String, color: Color32) {
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

    #[test]
    fn problems_name_the_field_they_belong_to() {
        let mut draft = NetworkStreamDraft::default();
        let problem = draft.checked().unwrap_err();
        assert_eq!(problem.field, StreamField::Url);
        assert!(problem.prompt, "an empty URL is still to be typed");
        draft.url = "rtsp://".into();
        assert_eq!(
            draft.checked().unwrap_err(),
            FieldProblem::error(StreamField::Url, Msg::NsReasonUrlInvalid.en())
        );
        draft.url = "rtsp://cam.local/live".into();
        draft.settings.reconnect.backoff_min_ms = 500;
        draft.settings.reconnect.backoff_max_ms = 400;
        assert_eq!(
            draft.checked().unwrap_err(),
            FieldProblem::error(StreamField::Backoff, Msg::NsReasonBackoff.en())
        );
        assert_eq!(StreamField::Backoff.label(), Msg::NsBackoffMax);
        draft.settings.reconnect.backoff_max_ms = 5_000;
        draft.url = "rtmp://0.0.0.0:1935/live".into();
        draft.stream_key = "obs key".into();
        assert_eq!(draft.checked().unwrap_err().field, StreamField::StreamKey);
        draft.url = "srt://0.0.0.0:9000".into();
        draft.passphrase = "short".into();
        assert_eq!(draft.checked().unwrap_err().field, StreamField::Passphrase);
    }

    #[test]
    fn only_the_url_password_is_masked() {
        let bullet = egui::epaint::text::PASSWORD_REPLACEMENT_CHAR;
        let url = "rtsp://operator:s3cret@10.0.0.42:554/stream1";
        let masked = mask_url_password(url);
        assert_eq!(
            masked,
            format!(
                "rtsp://operator:{}@10.0.0.42:554/stream1",
                bullet.to_string().repeat(6)
            )
        );
        assert_eq!(masked.chars().count(), url.chars().count());
        for plain in [
            "rtsp://cam.local/live",
            "rtsp://operator@cam.local/live",
            "rtsp://operator:@cam.local/live",
            "http://cam.local/a:b@c",
            "not a url",
        ] {
            assert_eq!(url_password(plain), None, "{plain}");
            assert_eq!(mask_url_password(plain), plain);
        }
        // The last `@` of the authority ends the password.
        assert_eq!(
            url_password("rtsp://u:p@ss@h/live").map(|range| &"rtsp://u:p@ss@h/live"[range]),
            Some("p@ss")
        );
    }
}
