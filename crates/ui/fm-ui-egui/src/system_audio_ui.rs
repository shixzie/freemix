//! System audio inputs in the console (AU-011): the Add input dialog's
//! system audio form, and the capture badge and details on an input's
//! tile.
//!
//! The form picks the audio system (`PulseAudio` / `PipeWire`, or ALSA) and
//! takes the device name as the operator types it, checked with the same
//! rule the daemon applies. Nothing on the server is enumerated, so each
//! audio system suggests a device: `@DEFAULT_MONITOR@` hears what the
//! server's default output plays, and `hw:Loopback,1` is the capture side
//! of an `snd-aloop` loopback. The tile badge reads the input's latest
//! `system_audio_status` (OPENING, LIVE, REOPENING n, FAILED); the tile's
//! tooltip carries the device, its audio system, the reopen count, and the
//! last error.

use egui::{Color32, Ui};
use fm_protocol::{
    MAX_SYSTEM_AUDIO_DEVICE_BYTES, SystemAudioBackend, SystemAudioConfig, SystemAudioEntry,
    SystemAudioState, SystemAudioStatus,
};

use crate::input_catalogue::kind_key;
use crate::l10n::{Arg, Locale, Msg};
use crate::network_stream_ui::{FieldProblem, field_error, hint_slot, mark_invalid};
use crate::theme::{AMBER, ERROR, muted_color, section_label};

/// The system audio form of the Add input dialog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SystemAudioDraft {
    pub(crate) backend: SystemAudioBackend,
    pub(crate) device: String,
    /// The operator typed the device; a backend change no longer suggests
    /// one.
    pub(crate) device_edited: bool,
}

impl Default for SystemAudioDraft {
    fn default() -> Self {
        let backend = SystemAudioBackend::Pulse;
        Self {
            backend,
            device: suggested_device(backend).to_owned(),
            device_edited: false,
        }
    }
}

/// The form field a problem belongs to: the device is the only one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemAudioField {
    Device,
}

impl SystemAudioField {
    pub(crate) const fn label(self) -> Msg {
        match self {
            Self::Device => Msg::SaDevice,
        }
    }
}

/// The device each audio system suggests.
const fn suggested_device(backend: SystemAudioBackend) -> &'static str {
    match backend {
        SystemAudioBackend::Pulse => "@DEFAULT_MONITOR@",
        SystemAudioBackend::Alsa => "hw:Loopback,1",
    }
}

impl SystemAudioDraft {
    /// Switches the audio system, suggesting its device unless the
    /// operator typed one.
    pub(crate) fn set_backend(&mut self, backend: SystemAudioBackend) {
        self.backend = backend;
        if !self.device_edited {
            suggested_device(backend).clone_into(&mut self.device);
        }
    }

    /// The configuration the daemon would accept, or why not (an English
    /// catalog reason).
    pub(crate) fn config(&self) -> Result<SystemAudioConfig, &'static str> {
        self.checked().map_err(|problem| problem.reason)
    }

    /// The configuration, or the field to fix and why.
    pub(crate) fn checked(&self) -> Result<SystemAudioConfig, FieldProblem<SystemAudioField>> {
        if self.device.is_empty() {
            return Err(FieldProblem::prompt(
                SystemAudioField::Device,
                Msg::SaReasonDeviceEmpty.en(),
            ));
        }
        let config = SystemAudioConfig::new(self.backend, self.device.clone());
        config.validate().map_err(|_| {
            FieldProblem::error(SystemAudioField::Device, Msg::SaReasonDeviceInvalid.en())
        })?;
        Ok(config)
    }
}

/// Draws the audio system keys and the device field.
pub(crate) fn draw_system_audio_form(ui: &mut Ui, l: Locale, draft: &mut SystemAudioDraft) {
    ui.label(section_label(l.text(Msg::SaBackend)));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (backend, label, hover) in [
            (
                SystemAudioBackend::Pulse,
                Msg::SaBackendPulse,
                Msg::SaBackendPulseHover,
            ),
            (
                SystemAudioBackend::Alsa,
                Msg::SaBackendAlsa,
                Msg::SaBackendAlsaHover,
            ),
        ] {
            let selected = draft.backend == backend;
            if ui
                .add(kind_key(l.text(label), selected))
                .on_hover_text(l.text(hover))
                .clicked()
                && !selected
            {
                draft.set_backend(backend);
            }
        }
    });
    ui.add_space(4.0);
    ui.label(section_label(l.text(Msg::SaDevice)));
    let edit = ui
        .add(
            egui::TextEdit::singleline(&mut draft.device)
                .id(egui::Id::new("studio-add-input-system-audio-device"))
                .char_limit(MAX_SYSTEM_AUDIO_DEVICE_BYTES)
                .desired_width(ui.available_width()),
        )
        .on_hover_text(l.text(Msg::SaDeviceHover));
    if edit.changed() {
        draft.device_edited = true;
    }
    let problem = draft.checked().err();
    if problem.is_some_and(|problem| !problem.prompt) {
        mark_invalid(ui, edit.rect);
    }
    hint_slot(
        ui,
        Some(match problem {
            Some(problem) if !problem.prompt => field_error(l, problem.reason),
            _ => crate::theme::caption(l.text(match draft.backend {
                SystemAudioBackend::Pulse => Msg::SaDevicePulseCaption,
                SystemAudioBackend::Alsa => Msg::SaDeviceAlsaCaption,
            })),
        }),
    );
}

/// The tile badge for a system audio status: its text and colour, or
/// `None` while nothing captures the device (idle).
pub(crate) fn system_audio_badge(
    l: Locale,
    status: &SystemAudioStatus,
) -> Option<(String, Color32)> {
    Some(match status.state {
        SystemAudioState::Idle => return None,
        SystemAudioState::Opening => (l.text(Msg::SaBadgeOpening).to_owned(), muted_color()),
        SystemAudioState::Live => (l.text(Msg::Live).to_owned(), Color32::from_rgb(30, 150, 70)),
        SystemAudioState::Reopening { attempt, .. } => (
            l.format(
                Msg::SaBadgeReopening,
                &[("attempt", Arg::from(u64::from(attempt)))],
            ),
            AMBER,
        ),
        SystemAudioState::Failed => (l.text(Msg::ConnFailed).to_owned(), ERROR),
    })
}

/// Paints the system audio badge in the picture's bottom trailing corner.
pub(crate) fn paint_system_audio_badge(
    painter: &egui::Painter,
    thumb: egui::Rect,
    l: Locale,
    status: &SystemAudioStatus,
) {
    if let Some((text, color)) = system_audio_badge(l, status) {
        crate::network_stream_ui::paint_corner_badge(painter, thumb, text, color);
    }
}

/// The tile tooltip line for a system audio input: the device and its
/// audio system, the state, the reopens, and the last error.
pub(crate) fn system_audio_details(l: Locale, entry: &SystemAudioEntry) -> String {
    let status = &entry.status;
    let state = system_audio_badge(l, status)
        .map_or_else(|| l.text(Msg::NsBadgeIdle).to_owned(), |(text, _)| text);
    let backend = l.text(match entry.config.backend {
        SystemAudioBackend::Pulse => Msg::SaBackendPulse,
        SystemAudioBackend::Alsa => Msg::SaBackendAlsa,
    });
    let mut lines = vec![l.format(
        Msg::SaDetail,
        &[
            ("device", Arg::Str(&entry.config.device)),
            ("backend", Arg::Str(backend)),
            ("state", Arg::Str(&state)),
        ],
    )];
    if status.reopens > 0 {
        lines.push(l.format(
            Msg::SaDetailReopens,
            &[("count", Arg::from(u64::from(status.reopens)))],
        ));
    }
    if let SystemAudioState::Reopening { next_in_ms, .. } = status.state {
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

#[cfg(test)]
mod tests {
    use fm_ui_model::i18n::Language;

    use super::*;

    #[test]
    fn the_form_suggests_a_device_and_validates_like_the_daemon() {
        let mut draft = SystemAudioDraft::default();
        assert_eq!(
            draft.config(),
            Ok(SystemAudioConfig::new(
                SystemAudioBackend::Pulse,
                "@DEFAULT_MONITOR@"
            ))
        );
        draft.set_backend(SystemAudioBackend::Alsa);
        assert_eq!(draft.device, "hw:Loopback,1");
        draft.device = "plughw:1,0".into();
        draft.device_edited = true;
        draft.set_backend(SystemAudioBackend::Pulse);
        assert_eq!(draft.device, "plughw:1,0", "a typed device stays");
        draft.device.clear();
        let problem = draft.checked().unwrap_err();
        assert!(problem.prompt);
        assert_eq!(problem.field.label(), Msg::SaDevice);
        draft.device = "-f lavfi".into();
        let problem = draft.checked().unwrap_err();
        assert!(!problem.prompt);
        assert_eq!(problem.reason, Msg::SaReasonDeviceInvalid.en());
    }

    #[test]
    fn badges_and_details_follow_the_status() {
        let l = Locale::new(Language::English);
        assert_eq!(system_audio_badge(l, &SystemAudioStatus::default()), None);
        let entry = SystemAudioEntry {
            input: fm_protocol::WireInputId::from_domain(fm_types::InputId::new(
                core::num::NonZeroU128::new(5).unwrap(),
            )),
            config: SystemAudioConfig::new(SystemAudioBackend::Alsa, "hw:Loopback,1"),
            status: SystemAudioStatus {
                state: SystemAudioState::Reopening {
                    attempt: 2,
                    next_in_ms: 1_500,
                },
                reopens: 2,
                last_error: Some("No such device".into()),
            },
        };
        assert_eq!(
            system_audio_badge(l, &entry.status).map(|(text, _)| text),
            Some("REOPENING 2".to_owned())
        );
        let details = system_audio_details(l, &entry);
        assert!(details.contains("hw:Loopback,1"), "{details}");
        assert!(details.contains("ALSA"), "{details}");
        assert!(details.contains("No such device"), "{details}");
    }
}
