//! System audio inputs in the console (AU-011): the Add input dialog's
//! system audio form, and the capture badge and details on an input's
//! tile.
//!
//! The form picks the audio system (`PulseAudio` / `PipeWire`, or ALSA) and
//! the device: from a picker of the devices the server lists (Protocol
//! 2.68, asked for when the form opens and on Refresh list), headed by the
//! audio system's Default device (`@DEFAULT_MONITOR@`, what the server's
//! default output plays, or ALSA's `default`), or typed into the name field
//! under it, which follows the picker and keeps any name the list does not
//! show. Either way the name is checked with the same rule the daemon
//! applies. The Replace source dialog uses the same form. The tile badge
//! reads the input's latest
//! `system_audio_status` (OPENING, LIVE, REOPENING n, FAILED); the tile's
//! tooltip carries the device, its audio system, the reopen count, and the
//! last error.

use egui::{Color32, ComboBox, RichText, Ui};
use fm_protocol::{
    MAX_SYSTEM_AUDIO_DEVICE_BYTES, SystemAudioBackend, SystemAudioConfig, SystemAudioDevice,
    SystemAudioEntry, SystemAudioInventory, SystemAudioState, SystemAudioStatus,
    default_system_audio_device,
};

use crate::StudioUiState;
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::input_catalogue::kind_key;
use crate::l10n::{Arg, Locale, Msg};
use crate::network_stream_ui::{
    FieldProblem, field_error, hint_slot, mark_invalid, section_heading_with,
};
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
            device: default_system_audio_device(backend).to_owned(),
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

/// What the device picker shows chosen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeviceChoice<'a> {
    /// The audio system's default device.
    Default,
    /// A device the server listed.
    Listed(&'a SystemAudioDevice),
    /// A typed name the server did not list.
    Typed,
}

impl SystemAudioDraft {
    /// A draft capturing `config`, for editing an input's device.
    pub(crate) fn of(config: &SystemAudioConfig) -> Self {
        Self {
            backend: config.backend,
            device: config.device.clone(),
            device_edited: config.device != default_system_audio_device(config.backend),
        }
    }

    /// Switches the audio system, choosing its default device unless the
    /// operator typed or picked another.
    pub(crate) fn set_backend(&mut self, backend: SystemAudioBackend) {
        self.backend = backend;
        if !self.device_edited {
            default_system_audio_device(backend).clone_into(&mut self.device);
        }
    }

    /// What the picker shows chosen, given the server's listed devices.
    pub(crate) fn choice<'a>(
        &self,
        inventory: Option<&'a SystemAudioInventory>,
    ) -> DeviceChoice<'a> {
        if self.device == default_system_audio_device(self.backend) {
            return DeviceChoice::Default;
        }
        inventory
            .and_then(|inventory| {
                inventory
                    .devices_of(self.backend)
                    .find(|device| device.device == self.device)
            })
            .map_or(DeviceChoice::Typed, DeviceChoice::Listed)
    }

    /// Picks the audio system's default device.
    pub(crate) fn choose_default(&mut self) {
        default_system_audio_device(self.backend).clone_into(&mut self.device);
        self.device_edited = false;
    }

    /// Picks a listed device (and its audio system).
    pub(crate) fn choose(&mut self, device: &SystemAudioDevice) {
        self.backend = device.backend;
        device.device.clone_into(&mut self.device);
        self.device_edited = true;
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

const fn backend_text(backend: SystemAudioBackend) -> Msg {
    match backend {
        SystemAudioBackend::Pulse => Msg::SaBackendPulse,
        SystemAudioBackend::Alsa => Msg::SaBackendAlsa,
    }
}

/// A listed device as the picker names it: its description (or its name),
/// then its channels and whether it is an output monitor or the default.
pub(crate) fn device_label(l: Locale, device: &SystemAudioDevice) -> String {
    let mut parts = vec![if device.description.is_empty() {
        device.device.clone()
    } else {
        device.description.clone()
    }];
    if let Some(channels) = device.channels {
        parts.push(l.format(
            Msg::SaTagChannels,
            &[("count", Arg::from(u64::from(channels)))],
        ));
    }
    if device.monitor {
        parts.push(l.text(Msg::SaTagMonitor).to_owned());
    }
    if device.default {
        parts.push(l.text(Msg::SaTagDefault).to_owned());
    }
    parts.join(" · ")
}

/// The line over the picker: the listing state for `backend`, and whether
/// it reads as a problem (amber).
pub(crate) fn list_line(
    l: Locale,
    backend: SystemAudioBackend,
    inventory: Option<&SystemAudioInventory>,
) -> (String, bool) {
    let Some(inventory) = inventory else {
        return (l.text(Msg::SaListUnknown).to_owned(), false);
    };
    if !inventory.supported {
        return (l.text(Msg::SaListUnsupported).to_owned(), true);
    }
    let name = l.text(backend_text(backend));
    if let Some(error) = inventory.error_of(backend) {
        return (
            l.format(
                Msg::SaListError,
                &[("backend", Arg::Str(name)), ("error", Arg::Str(error))],
            ),
            true,
        );
    }
    let count = inventory.devices_of(backend).count();
    if count == 0 {
        return (
            l.format(Msg::SaListEmpty, &[("backend", Arg::Str(name))]),
            true,
        );
    }
    (
        l.format(
            Msg::SaListCount,
            &[
                ("count", Arg::from(count as u64)),
                ("backend", Arg::Str(name)),
            ],
        ),
        false,
    )
}

/// The device picker: Default device, then the listed devices of the
/// draft's audio system; a choice fills the device name.
fn draw_device_picker(
    ui: &mut Ui,
    l: Locale,
    inventory: Option<&SystemAudioInventory>,
    id: &str,
    draft: &mut SystemAudioDraft,
) {
    let choice = draft.choice(inventory);
    let default_name = default_system_audio_device(draft.backend);
    let default_hover = l.format(
        match draft.backend {
            SystemAudioBackend::Pulse => Msg::SaPickDefaultPulseHover,
            SystemAudioBackend::Alsa => Msg::SaPickDefaultAlsaHover,
        },
        &[("device", Arg::Str(default_name))],
    );
    let selected = match choice {
        DeviceChoice::Default => l.text(Msg::SaPickDefault).to_owned(),
        DeviceChoice::Listed(device) => device_label(l, device),
        DeviceChoice::Typed => l.format(Msg::SaPickTyped, &[("device", Arg::Str(&draft.device))]),
    };
    let mut picked: Option<Option<SystemAudioDevice>> = None;
    ComboBox::from_id_salt(format!("{id}-system-audio-picker"))
        .width(ui.available_width())
        .truncate()
        .selected_text(selected)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(choice == DeviceChoice::Default, l.text(Msg::SaPickDefault))
                .on_hover_text(&default_hover)
                .clicked()
            {
                picked = Some(None);
            }
            for device in inventory
                .into_iter()
                .flat_map(|inventory| inventory.devices_of(draft.backend))
            {
                let chosen = choice == DeviceChoice::Listed(device);
                if ui
                    .selectable_label(chosen, device_label(l, device))
                    .on_hover_text(l.format(
                        Msg::SaDeviceOptionHover,
                        &[("device", Arg::Str(&device.device))],
                    ))
                    .clicked()
                {
                    picked = Some(Some(device.clone()));
                }
            }
        })
        .response
        .on_hover_text(l.text(Msg::SaPickerHover));
    match picked {
        Some(None) => draft.choose_default(),
        Some(Some(device)) => draft.choose(&device),
        None => {}
    }
}

/// Draws the audio system keys, the device picker with Refresh list, and
/// the device name field; `id` keeps the dialog's widgets apart. Returns
/// whether the operator asked for the device list again.
pub(crate) fn draw_system_audio_form(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    id: &str,
    draft: &mut SystemAudioDraft,
) -> bool {
    let mut rescan = false;
    // While the form stays open, refresh the list every few seconds so a
    // device appearing or going away shows up without the operator asking.
    const RESCAN_SECONDS: f64 = 5.0;
    let refreshed_at = ui.make_persistent_id(format!("{id}-system-audio-refreshed-at"));
    let now = ui.input(|input| input.time);
    let due = ui
        .data(|data| data.get_temp::<f64>(refreshed_at))
        .is_none_or(|last| now < last || now - last >= RESCAN_SECONDS);
    if due {
        ui.data_mut(|data| data.insert_temp(refreshed_at, now));
        rescan = true;
    }
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs_f64(RESCAN_SECONDS));
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
    let inventory = state
        .view
        .as_ref()
        .and_then(|view| view.system_audio_inventory.as_ref());
    section_heading_with(ui, l, Msg::SaDevice, |ui| {
        if ui
            .add_enabled(
                state.connection_status.controls_enabled(),
                egui::Button::new(
                    RichText::new(l.text(Msg::SaRefresh)).size(crate::theme::legible(11.0)),
                ),
            )
            .hint(
                l.text(Msg::SaRefreshHover),
                unavailable_reason(state, Capability::EditProject),
            )
            .clicked()
        {
            rescan = true;
        }
    });
    let (line, problem) = list_line(l, draft.backend, inventory);
    ui.label(
        RichText::new(line)
            .size(crate::theme::legible(10.5))
            .color(if problem { AMBER } else { muted_color() }),
    );
    draw_device_picker(ui, l, inventory, id, draft);
    ui.add_space(4.0);
    let edit = ui
        .add(
            egui::TextEdit::singleline(&mut draft.device)
                .id(egui::Id::new(format!("{id}-system-audio-device")))
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
    rescan
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
    let backend = l.text(backend_text(entry.config.backend));
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
        assert_eq!(draft.device, "default");
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

    fn listed(backend: SystemAudioBackend, device: &str) -> SystemAudioDevice {
        SystemAudioDevice {
            backend,
            device: device.into(),
            description: format!("{device} description"),
            channels: Some(2),
            default: false,
            monitor: false,
        }
    }

    fn inventory() -> SystemAudioInventory {
        let mut monitor = listed(SystemAudioBackend::Pulse, "sink.monitor");
        monitor.monitor = true;
        monitor.default = true;
        SystemAudioInventory {
            supported: true,
            devices: vec![
                monitor,
                listed(SystemAudioBackend::Alsa, "plughw:Loopback,1"),
            ],
            errors: Vec::new(),
        }
    }

    /// The picker shows the default, a listed device, or a typed name, and
    /// choosing from it fills the name field; a typed name stays possible.
    #[test]
    fn the_picker_follows_the_listed_devices_and_typed_names() {
        let inventory = inventory();
        let mut draft = SystemAudioDraft::default();
        assert_eq!(draft.choice(Some(&inventory)), DeviceChoice::Default);
        assert_eq!(draft.choice(None), DeviceChoice::Default);
        draft.choose(&inventory.devices[0]);
        assert_eq!(draft.device, "sink.monitor");
        assert!(draft.device_edited);
        assert_eq!(
            draft.choice(Some(&inventory)),
            DeviceChoice::Listed(&inventory.devices[0])
        );
        assert_eq!(draft.choice(None), DeviceChoice::Typed, "not listed yet");
        // A listed device of the other audio system switches to it.
        draft.choose(&inventory.devices[1]);
        assert_eq!(draft.backend, SystemAudioBackend::Alsa);
        assert_eq!(
            draft.config(),
            Ok(SystemAudioConfig::new(
                SystemAudioBackend::Alsa,
                "plughw:Loopback,1"
            ))
        );
        // Typing a name the server does not list keeps it.
        draft.device = "hw:CARD=Other,DEV=0".into();
        assert_eq!(draft.choice(Some(&inventory)), DeviceChoice::Typed);
        assert!(draft.config().is_ok());
        draft.choose_default();
        assert_eq!(draft.device, "default");
        assert!(!draft.device_edited);
        draft.set_backend(SystemAudioBackend::Pulse);
        assert_eq!(draft.device, "@DEFAULT_MONITOR@");
        // Editing an existing input starts from its device.
        let edited = SystemAudioDraft::of(&SystemAudioConfig::new(
            SystemAudioBackend::Pulse,
            "sink.monitor",
        ));
        assert_eq!(
            edited.choice(Some(&inventory)),
            DeviceChoice::Listed(&inventory.devices[0])
        );
        assert!(edited.device_edited);
    }

    #[test]
    fn the_list_line_and_labels_read_the_server_listing() {
        let l = Locale::new(Language::English);
        let inventory = inventory();
        assert_eq!(
            device_label(l, &inventory.devices[0]),
            "sink.monitor description · 2 channels · output monitor · system default"
        );
        let mut bare = inventory.devices[1].clone();
        bare.description.clear();
        bare.channels = Some(1);
        assert_eq!(device_label(l, &bare), "plughw:Loopback,1 · 1 channel");
        let pulse = SystemAudioBackend::Pulse;
        assert_eq!(
            list_line(l, pulse, None),
            (l.text(Msg::SaListUnknown).to_owned(), false)
        );
        assert_eq!(
            list_line(l, pulse, Some(&inventory)),
            (
                "The server lists 1 PulseAudio / PipeWire capture device".to_owned(),
                false
            )
        );
        let mut failed = inventory.clone();
        failed.devices.clear();
        failed.errors = vec![fm_protocol::SystemAudioBackendError {
            backend: pulse,
            error: "pactl is not installed".into(),
        }];
        assert_eq!(
            list_line(l, pulse, Some(&failed)),
            (
                "PulseAudio / PipeWire could not be listed: pactl is not installed".to_owned(),
                true
            )
        );
        assert!(
            list_line(l, SystemAudioBackend::Alsa, Some(&failed)).1,
            "none listed"
        );
        assert_eq!(
            list_line(l, pulse, Some(&SystemAudioInventory::unsupported())),
            (l.text(Msg::SaListUnsupported).to_owned(), true)
        );
        let es = Locale::new(Language::Spanish);
        assert_ne!(
            device_label(es, &inventory.devices[0]),
            device_label(l, &inventory.devices[0])
        );
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
