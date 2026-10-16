//! Camera and capture-card inputs in the console (IN-001): the Add input
//! dialog's capture form, and the acquisition badge and details on an
//! input's tile.
//!
//! The form lists the devices from the daemon's capture inventory (asked
//! for when the operator picks the Camera / capture kind, and on Rescan),
//! then the chosen device's formats, filterable by picture size, frame rate,
//! and pixel format and grouped by SD/HD/UHD class, or Automatic (the
//! daemon opens the format nearest the program). Audio is none, the
//! device's paired audio, or a named ALSA device; the loss slate holds the
//! last picture or shows black. The form validates as the operator types,
//! with the same rules the daemon applies.
//!
//! The tile badge reads the input's latest `capture_status`: SIGNAL LOST
//! (prominent, across the picture), LIVE, STARTING, or FAILED; the tile's
//! tooltip carries the device node, negotiated format, audio, loss,
//! recovery, and renegotiation counts, and the last error.

use std::collections::BTreeSet;

use egui::{
    Color32, ComboBox, CornerRadius, FontFamily, FontId, Painter, Rect, RichText, ScrollArea,
    Stroke, StrokeKind, Ui, Vec2, pos2,
};
use fm_protocol::{
    CaptureAudio, CaptureDevice, CaptureError, CaptureFormat, CaptureInputConfig,
    CaptureInputEntry, CaptureInventory, CaptureLossReason, CaptureLossSlate, CapturePixelFormat,
    CaptureScan, CaptureSettings, CaptureSignalState, CaptureStatus, validate_stable_key,
};
use fm_types::MAX_INPUT_NAME_BYTES;
use fm_types::capture::{MAX_ALSA_DEVICE_BYTES, ResolutionClass};

use crate::StudioUiState;
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::input_catalogue::kind_key;
use crate::l10n::{Arg, Locale, Msg};
use crate::theme::{AMBER, ERROR, mono, muted_color, section_label, text_dim_color};

/// The audio a new capture input hears, as the form offers it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum AudioChoice {
    None,
    #[default]
    Paired,
    /// The ALSA device typed in [`CaptureDraft::alsa_device`].
    Alsa,
}

/// The format list's filters; `None` shows every value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FormatFilter {
    pub(crate) size: Option<(u32, u32)>,
    /// Frames per second, in millihertz.
    pub(crate) rate: Option<u64>,
    pub(crate) pixel: Option<CapturePixelFormat>,
}

impl FormatFilter {
    fn admits(self, format: &CaptureFormat) -> bool {
        self.size
            .is_none_or(|size| size == (format.width, format.height))
            && self.rate.is_none_or(|rate| rate == format.millihertz())
            && self.pixel.is_none_or(|pixel| pixel == format.pixel_format)
    }
}

/// The capture form of the Add input dialog.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CaptureDraft {
    /// The chosen device's stable key.
    pub(crate) device: Option<String>,
    /// The requested format; `None` is Automatic.
    pub(crate) format: Option<CaptureFormat>,
    pub(crate) audio: AudioChoice,
    pub(crate) alsa_device: String,
    pub(crate) loss: CaptureLossSlate,
    pub(crate) filter: FormatFilter,
}

impl CaptureDraft {
    /// The configuration the form describes, or why the daemon would refuse
    /// it (an English catalog reason).
    pub(crate) fn config(&self) -> Result<CaptureInputConfig, &'static str> {
        let stable_key = self
            .device
            .as_deref()
            .filter(|key| validate_stable_key(key).is_ok())
            .ok_or(Msg::CapReasonNoDevice.en())?;
        let audio = match self.audio {
            AudioChoice::None => CaptureAudio::None,
            AudioChoice::Paired => CaptureAudio::Paired,
            AudioChoice::Alsa => CaptureAudio::Alsa(self.alsa_device.trim().to_owned()),
        };
        let config = CaptureInputConfig {
            stable_key: stable_key.to_owned(),
            settings: CaptureSettings {
                format: self.format,
                audio,
                loss: self.loss,
            },
        };
        config.validate().map_err(|error| match error {
            CaptureError::AlsaDevice => Msg::CapReasonAlsa.en(),
            CaptureError::StableKey => Msg::CapReasonNoDevice.en(),
            CaptureError::Dimensions
            | CaptureError::Rate
            | CaptureError::FormatToken
            | CaptureError::Token => Msg::CapReasonSettings.en(),
        })?;
        Ok(config)
    }

    /// Chooses `device`: a requested format it does not offer reverts to
    /// Automatic, and the filters reset.
    pub(crate) fn select_device(&mut self, device: &CaptureDevice) {
        if self.device.as_deref() != Some(device.stable_key.as_str()) {
            self.filter = FormatFilter::default();
        }
        self.device = Some(device.stable_key.clone());
        if self
            .format
            .is_some_and(|format| !device.formats.contains(&format))
        {
            self.format = None;
        }
    }
}

/// What the operator did in the capture form this frame.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CaptureFormOutcome {
    /// Rescan was pressed: ask the daemon for its devices again.
    pub(crate) rescan: bool,
    /// A device was chosen: its name, to suggest as the input name.
    pub(crate) picked: Option<String>,
}

/// A unique input name for a device: its name, else its name numbered.
pub(crate) fn suggested_name(device: &str, taken: &[&str]) -> Option<String> {
    let mut base = device.trim().to_owned();
    // Room for a ` NN` suffix within the name limit.
    let limit = MAX_INPUT_NAME_BYTES - 4;
    if base.len() > limit {
        let mut end = limit;
        while !base.is_char_boundary(end) {
            end -= 1;
        }
        base.truncate(end);
        let trimmed = base.trim_end().len();
        base.truncate(trimmed);
    }
    if base.is_empty() {
        return None;
    }
    if !taken.contains(&base.as_str()) {
        return Some(base);
    }
    (2..=taken.len() + 2)
        .map(|number| format!("{base} {number}"))
        .find(|name| !taken.contains(&name.as_str()))
}

/// The inventory status line: unknown, no backend, a failed scan, none
/// found, or how many were found.
pub(crate) fn inventory_line(l: Locale, inventory: Option<&CaptureInventory>) -> (String, bool) {
    let Some(inventory) = inventory else {
        return (l.text(Msg::CapInventoryUnknown).to_owned(), false);
    };
    let Some(backend) = inventory.backend.as_deref() else {
        return (l.text(Msg::CapInventoryNoBackend).to_owned(), true);
    };
    let mut line = if inventory.devices.is_empty() {
        l.format(Msg::CapInventoryEmpty, &[("backend", Arg::Str(backend))])
    } else {
        l.format(
            Msg::CapInventoryCount,
            &[
                ("count", Arg::from(inventory.devices.len() as u64)),
                ("backend", Arg::Str(backend)),
            ],
        )
    };
    if let Some(error) = &inventory.error {
        line.push_str(". ");
        line.push_str(&l.format(Msg::CapInventoryError, &[("error", Arg::Str(error))]));
        return (line, true);
    }
    (line, inventory.devices.is_empty())
}

/// A frame rate in frames per second: `30`, `29.97`.
fn rate_text(millihertz: u64) -> String {
    if millihertz.is_multiple_of(1_000) {
        (millihertz / 1_000).to_string()
    } else {
        let hundredths = (millihertz + 5) / 10;
        format!("{}.{:02}", hundredths / 100, hundredths % 100)
    }
}

const fn scan_text(scan: CaptureScan) -> Msg {
    match scan {
        CaptureScan::Progressive => Msg::CapScanProgressive,
        CaptureScan::InterlacedTff => Msg::CapScanTff,
        CaptureScan::InterlacedBff => Msg::CapScanBff,
    }
}

/// The help of one format in the picker.
pub(crate) fn format_hover(l: Locale, format: &CaptureFormat) -> String {
    l.format(
        Msg::CapFormatOptionHover,
        &[
            ("format", Arg::Str(&format.label())),
            ("class", Arg::Str(format.class().label())),
            ("scan", Arg::Str(l.text(scan_text(format.scan)))),
        ],
    )
}

/// Draws the capture form: the device list, then for the chosen device its
/// format picker and audio, and the loss slate, each with its help.
pub(crate) fn draw_capture_form(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    draft: &mut CaptureDraft,
) -> CaptureFormOutcome {
    let mut outcome = CaptureFormOutcome::default();
    let inventory = state
        .view
        .as_ref()
        .and_then(|view| view.capture_inventory.as_ref());
    // A device the server no longer lists cannot be chosen.
    if let (Some(inventory), Some(key)) = (inventory, draft.device.as_deref())
        && !inventory
            .devices
            .iter()
            .any(|device| device.stable_key == key)
    {
        draft.device = None;
        draft.format = None;
    }
    ui.horizontal(|ui| {
        ui.label(section_label(l.text(Msg::CapDevice)));
        ui.with_layout(crate::l10n::trailing_layout(l), |ui| {
            if ui
                .add_enabled(
                    state.connection_status.controls_enabled(),
                    egui::Button::new(
                        RichText::new(l.text(Msg::CapRefresh)).size(crate::theme::legible(11.0)),
                    ),
                )
                .hint(
                    l.text(Msg::CapRefreshHover),
                    unavailable_reason(state, Capability::EditProject),
                )
                .clicked()
            {
                outcome.rescan = true;
            }
        });
    });
    let (line, problem) = inventory_line(l, inventory);
    ui.label(
        RichText::new(line)
            .size(crate::theme::legible(10.5))
            .color(if problem { AMBER } else { muted_color() }),
    );
    let devices = inventory.map_or(&[][..], |inventory| inventory.devices.as_slice());
    outcome.picked = draw_devices(ui, l, devices, draft);
    let device = draft.device.as_deref().and_then(|key| {
        devices
            .iter()
            .find(|device| device.stable_key.as_str() == key)
    });
    if let Some(device) = device {
        ui.add_space(4.0);
        draw_format_picker(ui, l, device, draft);
        ui.add_space(4.0);
        draw_audio(ui, l, device, draft);
    }
    ui.add_space(4.0);
    ui.label(section_label(l.text(Msg::CapLoss)));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (loss, label, hover) in [
            (
                CaptureLossSlate::Hold,
                Msg::CapLossHold,
                Msg::CapLossHoldHover,
            ),
            (
                CaptureLossSlate::Black,
                Msg::FtbWordBlack,
                Msg::CapLossBlackHover,
            ),
        ] {
            let selected = draft.loss == loss;
            if ui
                .add(kind_key(l.text(label), selected))
                .on_hover_text(l.text(hover))
                .clicked()
            {
                draft.loss = loss;
            }
        }
    });
    outcome
}

/// Lists the devices to choose from; returns the name of one just chosen.
fn draw_devices(
    ui: &mut Ui,
    l: Locale,
    devices: &[CaptureDevice],
    draft: &mut CaptureDraft,
) -> Option<String> {
    let mut picked = None;
    for device in devices {
        let chosen = draft.device.as_deref() == Some(device.stable_key.as_str());
        let paired = device
            .audio
            .as_deref()
            .unwrap_or_else(|| l.text(Msg::CapPairedNone));
        let response = ui
            .selectable_label(
                chosen,
                l.format(
                    Msg::CapDeviceOption,
                    &[
                        ("name", Arg::Str(&device.name)),
                        ("path", Arg::Str(&device.path)),
                        ("bus", Arg::Str(&device.bus)),
                    ],
                ),
            )
            .on_hover_text(l.format(
                Msg::CapDeviceHover,
                &[
                    ("name", Arg::Str(&device.name)),
                    ("path", Arg::Str(&device.path)),
                    ("bus", Arg::Str(&device.bus)),
                    ("driver", Arg::Str(&device.driver)),
                    ("formats", Arg::from(device.formats.len() as u64)),
                    ("audio", Arg::Str(paired)),
                ],
            ));
        if response.clicked() && !chosen {
            draft.select_device(device);
            picked = Some(device.name.clone());
        }
    }
    picked
}

/// One filter: Any, or one of `values`.
fn filter_combo<T: Copy + PartialEq>(
    ui: &mut Ui,
    l: Locale,
    (salt, hover): (&str, Msg),
    current: &mut Option<T>,
    values: &[T],
    text: &dyn Fn(T) -> String,
) {
    let all = l.text(Msg::CapFilterAll);
    ComboBox::from_id_salt(salt)
        .width(104.0)
        .truncate()
        .selected_text(current.map_or_else(|| all.to_owned(), text))
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(current.is_none(), all)
                .on_hover_text(l.text(hover))
                .clicked()
            {
                *current = None;
            }
            for &value in values {
                if ui
                    .selectable_label(*current == Some(value), text(value))
                    .on_hover_text(l.text(hover))
                    .clicked()
                {
                    *current = Some(value);
                }
            }
        })
        .response
        .on_hover_text(l.text(hover));
}

/// The size, rate, and pixel format filters over `device`'s formats.
fn draw_filters(ui: &mut Ui, l: Locale, device: &CaptureDevice, filter: &mut FormatFilter) {
    let mut sizes: Vec<(u32, u32)> = device
        .formats
        .iter()
        .map(|format| (format.width, format.height))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    sizes.sort_by_key(|&(width, height)| std::cmp::Reverse(u64::from(width) * u64::from(height)));
    let rates: Vec<u64> = device
        .formats
        .iter()
        .map(CaptureFormat::millihertz)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .rev()
        .collect();
    let pixels: Vec<CapturePixelFormat> = device
        .formats
        .iter()
        .map(|format| format.pixel_format)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    egui::Grid::new("studio-add-input-capture-filters")
        .num_columns(3)
        .spacing(Vec2::new(8.0, 2.0))
        .show(ui, |ui| {
            for (label, hover) in [
                (Msg::CapFilterSize, Msg::CapFilterSizeHover),
                (Msg::CapFilterRate, Msg::CapFilterRateHover),
                (Msg::CapFilterPixel, Msg::CapFilterPixelHover),
            ] {
                ui.label(
                    RichText::new(l.text(label))
                        .size(crate::theme::legible(10.5))
                        .color(text_dim_color()),
                )
                .on_hover_text(l.text(hover));
            }
            ui.end_row();
            filter_combo(
                ui,
                l,
                ("studio-add-input-capture-size", Msg::CapFilterSizeHover),
                &mut filter.size,
                &sizes,
                &|(width, height)| format!("{width}x{height}"),
            );
            filter_combo(
                ui,
                l,
                ("studio-add-input-capture-rate", Msg::CapFilterRateHover),
                &mut filter.rate,
                &rates,
                &|rate| l.format(Msg::CapRateFps, &[("rate", Arg::Str(&rate_text(rate)))]),
            );
            filter_combo(
                ui,
                l,
                ("studio-add-input-capture-pixel", Msg::CapFilterPixelHover),
                &mut filter.pixel,
                &pixels,
                &|pixel| pixel.label().to_owned(),
            );
            ui.end_row();
        });
}

/// The format picker: Automatic, or one of the device's formats that pass
/// the filters, grouped by resolution class.
fn draw_format_picker(ui: &mut Ui, l: Locale, device: &CaptureDevice, draft: &mut CaptureDraft) {
    ui.label(section_label(l.text(Msg::CapFormat)));
    draw_filters(ui, l, device, &mut draft.filter);
    let mut formats: Vec<CaptureFormat> = device
        .formats
        .iter()
        .copied()
        .filter(|format| draft.filter.admits(format))
        .collect();
    formats.sort_by_key(|format| {
        (
            std::cmp::Reverse(u64::from(format.width) * u64::from(format.height)),
            std::cmp::Reverse(format.millihertz()),
            format.pixel_format,
            format.scan,
        )
    });
    ScrollArea::vertical()
        .id_salt("studio-add-input-capture-formats")
        .max_height(150.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if ui
                .selectable_label(draft.format.is_none(), l.text(Msg::CapFormatAuto))
                .on_hover_text(l.text(Msg::CapFormatAutoHover))
                .clicked()
            {
                draft.format = None;
            }
            if formats.is_empty() {
                ui.label(
                    RichText::new(l.text(Msg::CapFilterNone))
                        .size(crate::theme::legible(10.5))
                        .color(muted_color()),
                );
            }
            for class in [
                ResolutionClass::Uhd,
                ResolutionClass::Hd,
                ResolutionClass::Sd,
            ] {
                let mut group = formats
                    .iter()
                    .filter(|format| format.class() == class)
                    .peekable();
                if group.peek().is_none() {
                    continue;
                }
                ui.label(
                    RichText::new(class.label())
                        .size(crate::theme::legible(9.5))
                        .strong()
                        .color(muted_color()),
                );
                for format in group {
                    let selected = draft.format == Some(*format);
                    if ui
                        .selectable_label(selected, mono(format.label()))
                        .on_hover_text(format_hover(l, format))
                        .clicked()
                    {
                        draft.format = Some(*format);
                    }
                }
            }
        });
}

fn draw_audio(ui: &mut Ui, l: Locale, device: &CaptureDevice, draft: &mut CaptureDraft) {
    ui.label(section_label(l.text(Msg::StingerAudio)));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
        for (audio, label, hover) in [
            (AudioChoice::None, Msg::CapAudioNone, Msg::CapAudioNoneHover),
            (
                AudioChoice::Paired,
                Msg::CapAudioPaired,
                Msg::CapAudioPairedHover,
            ),
            (AudioChoice::Alsa, Msg::CapAudioAlsa, Msg::CapAudioAlsaHover),
        ] {
            let selected = draft.audio == audio;
            if ui
                .add(kind_key(l.text(label), selected))
                .on_hover_text(l.text(hover))
                .clicked()
            {
                draft.audio = audio;
            }
        }
    });
    match draft.audio {
        AudioChoice::None => {}
        AudioChoice::Paired => {
            let paired = device
                .audio
                .as_deref()
                .unwrap_or_else(|| l.text(Msg::CapPairedNone));
            ui.label(
                RichText::new(l.format(Msg::CapPairedAudio, &[("audio", Arg::Str(paired))]))
                    .size(crate::theme::legible(10.5))
                    .color(muted_color()),
            );
        }
        AudioChoice::Alsa => {
            ui.add(
                egui::TextEdit::singleline(&mut draft.alsa_device)
                    .id(egui::Id::new("studio-add-input-capture-alsa"))
                    .hint_text("hw:CARD=C920,DEV=0")
                    .char_limit(MAX_ALSA_DEVICE_BYTES)
                    .desired_width(ui.available_width()),
            )
            .on_hover_text(l.text(Msg::CapAlsaDeviceHover));
            if CaptureAudio::Alsa(draft.alsa_device.trim().to_owned())
                .validate()
                .is_err()
            {
                ui.label(
                    RichText::new(l.text(Msg::CapReasonAlsa))
                        .size(crate::theme::legible(10.5))
                        .color(if draft.alsa_device.trim().is_empty() {
                            muted_color()
                        } else {
                            ERROR
                        }),
                );
            }
        }
    }
}

const fn loss_reason(reason: CaptureLossReason) -> Msg {
    match reason {
        CaptureLossReason::Unplugged => Msg::CapLostUnplugged,
        CaptureLossReason::Stalled => Msg::CapLostStalled,
        CaptureLossReason::NoSignal => Msg::CapLostNoSignal,
        CaptureLossReason::Error => Msg::CapLostError,
    }
}

/// The tile badge for a capture status: its text and colour, or `None`
/// while nothing acquires the device (idle).
pub(crate) fn capture_badge(l: Locale, status: &CaptureStatus) -> Option<(String, Color32)> {
    Some(match status.state {
        CaptureSignalState::Idle => return None,
        CaptureSignalState::Starting => (l.text(Msg::StreamStarting).to_owned(), AMBER),
        CaptureSignalState::Live => (l.text(Msg::Live).to_owned(), Color32::from_rgb(30, 150, 70)),
        CaptureSignalState::SignalLost => (l.text(Msg::CapBadgeSignalLost).to_owned(), ERROR),
        CaptureSignalState::Failed => (l.text(Msg::ConnFailed).to_owned(), ERROR),
    })
}

/// The tile tooltip line for a capture input: the tile help stays within
/// its four lines, so the details share one.
pub(crate) fn capture_details(l: Locale, entry: &CaptureInputEntry) -> String {
    let status = &entry.status;
    let mut state = capture_badge(l, status)
        .map_or_else(|| l.text(Msg::NsBadgeIdle).to_owned(), |(text, _)| text);
    if status.state == CaptureSignalState::SignalLost
        && let Some(reason) = status.reason
    {
        state = l.format(
            Msg::CapDetailLost,
            &[
                ("state", Arg::Str(&state)),
                ("reason", Arg::Str(l.text(loss_reason(reason)))),
            ],
        );
    }
    let device = status
        .device
        .as_deref()
        .unwrap_or(entry.config.stable_key.as_str());
    let format = status.format.map_or_else(
        || l.text(Msg::CapDetailNoFormat).to_owned(),
        |format| format.label(),
    );
    let mut lines = vec![
        l.format(
            Msg::CapDetailDevice,
            &[("device", Arg::Str(device)), ("state", Arg::Str(&state))],
        ),
        l.format(
            Msg::CapDetailFormat,
            &[
                ("format", Arg::Str(&format)),
                (
                    "audio",
                    Arg::Str(l.text(if status.audio {
                        Msg::CapDetailAudioOn
                    } else {
                        Msg::CapDetailAudioOff
                    })),
                ),
            ],
        ),
        l.format(
            Msg::CapDetailCounters,
            &[
                ("losses", Arg::from(u64::from(status.losses))),
                ("recoveries", Arg::from(u64::from(status.recoveries))),
                (
                    "renegotiations",
                    Arg::from(u64::from(status.renegotiations)),
                ),
            ],
        ),
    ];
    if let Some(error) = &status.last_error {
        lines.push(l.format(Msg::CardLastError, &[("error", Arg::Str(error))]));
    }
    lines.join(" · ")
}

/// Paints the capture badge on the picture: SIGNAL LOST as a banner across
/// its middle with an error outline, any other state in the bottom
/// trailing corner.
pub(crate) fn paint_capture_badge(
    painter: &Painter,
    thumb: Rect,
    l: Locale,
    status: &CaptureStatus,
) {
    let Some((text, color)) = capture_badge(l, status) else {
        return;
    };
    if status.state == CaptureSignalState::SignalLost {
        let galley = painter.layout_no_wrap(
            text,
            FontId::new(12.0, FontFamily::Proportional),
            Color32::WHITE,
        );
        let inner = thumb.shrink(2.0);
        painter.rect_stroke(
            inner,
            CornerRadius::same(3),
            Stroke::new(2.0, color),
            StrokeKind::Inside,
        );
        let banner = Rect::from_center_size(
            inner.center(),
            Vec2::new(
                (galley.size().x + 16.0).min(inner.width()),
                galley.size().y + 6.0,
            ),
        );
        painter.rect_filled(banner, CornerRadius::same(3), color);
        painter.galley(
            banner.center() - galley.size() / 2.0,
            galley,
            Color32::WHITE,
        );
        return;
    }
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
    use fm_types::FrameRate;
    use fm_ui_model::i18n::Language;

    use super::*;

    fn format(width: u32, height: u32, rate: (u32, u32), scan: CaptureScan) -> CaptureFormat {
        CaptureFormat {
            width,
            height,
            rate: FrameRate::new(rate.0, rate.1).unwrap(),
            pixel_format: CapturePixelFormat::Yuyv,
            scan,
        }
    }

    fn device() -> CaptureDevice {
        CaptureDevice {
            stable_key: "v4l2:usb-0000:00:14.0-1:C920".into(),
            name: "HD Pro Webcam C920".into(),
            path: "/dev/video0".into(),
            driver: "uvcvideo".into(),
            bus: "usb-0000:00:14.0-1".into(),
            audio: Some("hw:CARD=C920,DEV=0".into()),
            formats: vec![
                format(1920, 1080, (30, 1), CaptureScan::Progressive),
                format(1280, 720, (60, 1), CaptureScan::Progressive),
            ],
        }
    }

    #[test]
    fn the_form_builds_the_capture_source_and_validates_like_the_daemon() {
        let mut draft = CaptureDraft::default();
        assert_eq!(draft.config(), Err(Msg::CapReasonNoDevice.en()));
        let device = device();
        draft.select_device(&device);
        let config = draft.config().unwrap();
        assert_eq!(config.stable_key, device.stable_key);
        assert_eq!(config.settings, CaptureSettings::default());

        draft.format = Some(device.formats[1]);
        draft.audio = AudioChoice::Alsa;
        draft.alsa_device = " hw:CARD=USB,DEV=0 ".into();
        draft.loss = CaptureLossSlate::Black;
        let config = draft.config().unwrap();
        assert_eq!(
            config.settings,
            CaptureSettings {
                format: Some(device.formats[1]),
                audio: CaptureAudio::Alsa("hw:CARD=USB,DEV=0".into()),
                loss: CaptureLossSlate::Black,
            }
        );

        for bad in ["", "hw card", &"x".repeat(MAX_ALSA_DEVICE_BYTES + 1)] {
            draft.alsa_device = bad.to_owned();
            assert_eq!(draft.config(), Err(Msg::CapReasonAlsa.en()), "{bad:?}");
        }
        draft.audio = AudioChoice::None;
        assert_eq!(draft.config().unwrap().settings.audio, CaptureAudio::None);

        // A format the device does not offer reverts to Automatic.
        draft.format = Some(format(720, 576, (25, 1), CaptureScan::InterlacedTff));
        draft.filter.size = Some((1920, 1080));
        let mut other = device.clone();
        other.stable_key = "v4l2:other".into();
        draft.select_device(&other);
        assert_eq!(draft.format, None);
        assert_eq!(draft.filter, FormatFilter::default());
        draft.device = Some("\u{7}".into());
        assert_eq!(draft.config(), Err(Msg::CapReasonNoDevice.en()));
    }

    #[test]
    fn filters_admit_matching_formats_only() {
        let interlaced = format(1920, 1080, (30_000, 1_001), CaptureScan::InterlacedTff);
        assert_eq!(interlaced.label(), "1920x1080 59.94i YUYV");
        let mut filter = FormatFilter::default();
        assert!(filter.admits(&interlaced));
        filter.rate = Some(interlaced.millihertz());
        assert!(filter.admits(&interlaced));
        filter.pixel = Some(CapturePixelFormat::Mjpeg);
        assert!(!filter.admits(&interlaced));
        filter.pixel = None;
        filter.size = Some((1280, 720));
        assert!(!filter.admits(&interlaced));
        assert_eq!(rate_text(29_970), "29.97");
        assert_eq!(rate_text(60_000), "60");
        assert!(format_hover(Locale::ENGLISH, &interlaced).contains("HD, interlaced, top field"));
    }

    #[test]
    fn suggested_names_are_the_device_name_made_unique() {
        assert_eq!(
            suggested_name(" HD Pro Webcam C920 ", &["Camera 1"]).as_deref(),
            Some("HD Pro Webcam C920")
        );
        assert_eq!(
            suggested_name("Cam", &["Cam", "Cam 2"]).as_deref(),
            Some("Cam 3")
        );
        assert_eq!(suggested_name("  ", &[]), None);
        let long = "é".repeat(MAX_INPUT_NAME_BYTES);
        assert!(suggested_name(&long, &[]).unwrap().len() <= MAX_INPUT_NAME_BYTES - 4);
    }

    #[test]
    fn the_inventory_line_names_every_state() {
        let l = Locale::ENGLISH;
        assert_eq!(inventory_line(l, None).0, l.text(Msg::CapInventoryUnknown));
        let none = CaptureInventory::default();
        assert_eq!(
            inventory_line(l, Some(&none)),
            (
                "This server has no capture backend: it cannot open cameras or capture cards"
                    .into(),
                true
            )
        );
        let mut inventory = CaptureInventory {
            backend: Some("v4l2".into()),
            error: None,
            devices: Vec::new(),
        };
        assert!(
            inventory_line(l, Some(&inventory))
                .0
                .contains("No capture devices found (v4l2)")
        );
        inventory.devices.push(device());
        assert_eq!(
            inventory_line(l, Some(&inventory)),
            ("1 capture device found (v4l2)".into(), false)
        );
        inventory.error = Some("permission denied".into());
        let (line, problem) = inventory_line(l, Some(&inventory));
        assert!(
            line.ends_with("Device scan failed: permission denied"),
            "{line}"
        );
        assert!(problem);
    }

    #[test]
    fn badges_name_each_acquisition_state_in_both_languages() {
        let spanish = Locale::new(Language::Spanish);
        let status = |state| CaptureStatus {
            state,
            ..CaptureStatus::default()
        };
        let english = |state| capture_badge(Locale::ENGLISH, &status(state)).map(|badge| badge.0);
        assert_eq!(english(CaptureSignalState::Idle), None);
        assert_eq!(english(CaptureSignalState::Starting).unwrap(), "STARTING");
        assert_eq!(english(CaptureSignalState::Live).unwrap(), "LIVE");
        assert_eq!(english(CaptureSignalState::Failed).unwrap(), "FAILED");
        let lost = capture_badge(Locale::ENGLISH, &status(CaptureSignalState::SignalLost)).unwrap();
        assert_eq!(lost, ("SIGNAL LOST".to_owned(), ERROR));
        assert_eq!(
            capture_badge(spanish, &status(CaptureSignalState::SignalLost))
                .unwrap()
                .0,
            "SIN SEÑAL"
        );

        let entry = CaptureInputEntry {
            input: fm_protocol::WireInputId::new(core::num::NonZeroU128::new(4).unwrap()),
            config: CaptureInputConfig::new("v4l2:usb-1:C920"),
            status: CaptureStatus {
                state: CaptureSignalState::SignalLost,
                reason: Some(CaptureLossReason::Unplugged),
                format: Some(format(
                    1920,
                    1080,
                    (30_000, 1_001),
                    CaptureScan::InterlacedTff,
                )),
                audio: true,
                losses: 2,
                recoveries: 1,
                renegotiations: 3,
                last_error: Some("No such device".into()),
                ..CaptureStatus::default()
            },
        };
        let details = capture_details(Locale::ENGLISH, &entry);
        for part in [
            "Capture v4l2:usb-1:C920: SIGNAL LOST (device unplugged)",
            "1920x1080 59.94i YUYV, audio on",
            "2 losses, 1 recoveries, 3 renegotiations",
            "Last error: No such device",
        ] {
            assert!(details.contains(part), "{details}");
        }
        assert!(!details.contains('\n'), "{details}");
        let live = CaptureInputEntry {
            status: CaptureStatus {
                state: CaptureSignalState::Live,
                device: Some("/dev/video2".into()),
                ..CaptureStatus::default()
            },
            ..entry
        };
        let details = capture_details(spanish, &live);
        assert!(
            details.starts_with("Captura /dev/video2: VIVO"),
            "{details}"
        );
        assert!(
            details.contains("formato sin negociar, audio desactivado"),
            "{details}"
        );
    }
}
