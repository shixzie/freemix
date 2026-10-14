//! Stinger slot assignment and clearing.
//!
//! Slots are configured with the existing `configure_stinger` /
//! `remove_stinger` protocol commands, which the daemon admits under the
//! `transition` permission and rejects during a manual transition. Studio
//! exposes only the media assignment; a configured slot keeps its other
//! settings (preload, cut point, audio policy, missing-media fallback), and an
//! unconfigured slot receives fixed defaults resolved by the native app.

use egui::{Button, ComboBox, RichText, Stroke, Ui, Vec2};
use fm_protocol::{StingerAudioPolicy, StingerMissingMediaFallback, WireStingerSlotId};
use fm_types::InputId;
use fm_ui_model::{ClientView, StingerStatus};

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::panels::{stinger_slot_presentation, stinger_slot_state, stinger_slot_tone};
use crate::theme::{
    ACCENT, ACCENT_FILL, BORDER_STRONG, MUTED, TEXT, TEXT_DIM, WIDGET, card, mono, section_label,
    status_chip,
};
use crate::{StudioIntent, StudioUiState, TransitionGate, transition_availability};

/// Number of Stinger slots the protocol addresses.
pub(crate) const STINGER_SLOT_COUNT: u8 = 8;

/// Studio-local selection for the Stinger configuration row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StingerEditor {
    /// The one-based slot being configured.
    pub(crate) slot: u8,
    /// The explicitly chosen media input, if any.
    pub(crate) media: Option<InputId>,
}

impl Default for StingerEditor {
    fn default() -> Self {
        Self {
            slot: 1,
            media: None,
        }
    }
}

/// The default cut point Studio uses for an unconfigured slot: half of the
/// shared transition duration, so firing at that duration is always valid.
#[must_use]
pub const fn default_stinger_cut_point_frames(duration_frames: u32) -> u32 {
    duration_frames / 2
}

/// Whether Stinger slot configuration is available: control-ready, synchronized,
/// the `transition` permission, and no manual transition in flight.
#[must_use]
pub const fn stinger_configuration_available(state: &StudioUiState) -> bool {
    transition_availability(TransitionGate::from_state(state)).basic()
}

/// Returns the assignment intent when the session allows Stinger configuration
/// and `media_input` is an input of the replicated view.
#[must_use]
pub fn stinger_assign_intent(
    state: &StudioUiState,
    slot: WireStingerSlotId,
    media_input: InputId,
    duration_frames: u32,
) -> Option<StudioIntent> {
    let view = state.view.as_ref()?;
    (stinger_configuration_available(state) && view.inputs.contains(&media_input)).then_some(
        StudioIntent::AssignStingerMedia {
            slot,
            media_input,
            default_cut_point_frames: default_stinger_cut_point_frames(duration_frames),
        },
    )
}

/// Returns the clear intent when the session allows Stinger configuration and
/// the slot is configured in the replicated view.
#[must_use]
pub fn stinger_remove_intent(
    state: &StudioUiState,
    slot: WireStingerSlotId,
) -> Option<StudioIntent> {
    let view = state.view.as_ref()?;
    (stinger_configuration_available(state) && configured_stinger(view, slot.number()).is_some())
        .then_some(StudioIntent::RemoveStinger { slot })
}

/// The replicated descriptor of one configured slot.
pub(crate) fn configured_stinger(view: &ClientView, slot: u8) -> Option<&StingerStatus> {
    view.stingers.iter().find(|status| status.slot == slot)
}

/// The media the configuration row would assign: the operator's explicit
/// choice while it still exists, else the slot's current media, else Preview.
pub(crate) fn stinger_editor_media(view: &ClientView, editor: StingerEditor) -> Option<InputId> {
    editor
        .media
        .filter(|input| view.inputs.contains(input))
        .or_else(|| {
            configured_stinger(view, editor.slot)
                .map(|status| status.media_input)
                .filter(|input| view.inputs.contains(input))
        })
        .or_else(|| {
            let preview = view.switcher.desired.preview;
            view.inputs.contains(&preview).then_some(preview)
        })
}

fn input_name(view: &ClientView, input: InputId) -> String {
    view.inputs
        .iter()
        .position(|candidate| *candidate == input)
        .and_then(|index| view.input_names.get(index).cloned())
        .unwrap_or_else(|| format!("Input {input}"))
}

/// One-line summary of a slot's replicated configuration.
pub(crate) fn stinger_summary(view: &ClientView, slot: u8) -> String {
    let Some(status) = configured_stinger(view, slot) else {
        return format!("S{slot} is not configured");
    };
    format!(
        "S{slot}: {} · cut {} fr · {} · {} audio · fallback {}",
        input_name(view, status.media_input),
        status.cut_point_frames,
        if status.preload {
            "preload"
        } else {
            "no preload"
        },
        match status.audio_policy {
            StingerAudioPolicy::Muted => "muted",
            StingerAudioPolicy::StingerOnly => "stinger-only",
            StingerAudioPolicy::MixWithProgram => "mixed",
        },
        match status.missing_media_fallback {
            StingerMissingMediaFallback::Cut => "cut",
            StingerMissingMediaFallback::Fade => "fade",
            StingerMissingMediaFallback::KeepProgram => "keep program",
        },
    )
}

/// The slot and media pickers of the configuration row.
fn draw_stinger_pickers(
    ui: &mut Ui,
    view: &ClientView,
    editor: &mut StingerEditor,
    media: Option<InputId>,
) {
    let stingers = Some(view.stingers.as_slice());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ComboBox::from_id_salt("studio-stinger-config-slot")
            .width(118.0)
            .selected_text(
                mono(format!(
                    "S{} · {}",
                    editor.slot,
                    stinger_slot_presentation(stinger_slot_state(stingers, editor.slot)).label
                ))
                .size(10.5),
            )
            .show_ui(ui, |ui| {
                for slot in 1..=STINGER_SLOT_COUNT {
                    let label = stinger_slot_presentation(stinger_slot_state(stingers, slot)).label;
                    if ui
                        .selectable_label(editor.slot == slot, format!("S{slot} · {label}"))
                        .clicked()
                    {
                        editor.slot = slot;
                        editor.media = None;
                    }
                }
            })
            .response
            .on_hover_text("Stinger slot to configure");
        ComboBox::from_id_salt("studio-stinger-config-media")
            .width(ui.available_width() - 8.0)
            .selected_text(
                RichText::new(media.map_or_else(|| "no input".to_owned(), |m| input_name(view, m)))
                    .size(11.0),
            )
            .show_ui(ui, |ui| {
                for (input, name) in view.inputs.iter().zip(&view.input_names) {
                    if ui.selectable_label(media == Some(*input), name).clicked() {
                        editor.media = Some(*input);
                    }
                }
            })
            .response
            .on_hover_text(
                "Media input the slot plays (defaults to the slot's media, else Preview)",
            );
    });
}

/// Labelled facts about one slot for the Configure section: the replicated
/// settings of a configured slot, or the defaults ASSIGN applies to an empty one.
pub(crate) fn stinger_details(
    view: &ClientView,
    slot: u8,
    duration_frames: u32,
) -> [(&'static str, String); 4] {
    let Some(status) = configured_stinger(view, slot) else {
        return [
            (
                "Cut point",
                format!(
                    "{} fr (default)",
                    default_stinger_cut_point_frames(duration_frames)
                ),
            ),
            ("Preload", "yes (default)".to_owned()),
            ("Audio", "mixed with Program (default)".to_owned()),
            ("Fallback", "cut (default)".to_owned()),
        ];
    };
    [
        ("Cut point", format!("{} fr", status.cut_point_frames)),
        (
            "Preload",
            if status.preload { "yes" } else { "no" }.to_owned(),
        ),
        (
            "Audio",
            match status.audio_policy {
                StingerAudioPolicy::Muted => "muted",
                StingerAudioPolicy::StingerOnly => "stinger only",
                StingerAudioPolicy::MixWithProgram => "mixed with Program",
            }
            .to_owned(),
        ),
        (
            "Fallback",
            match status.missing_media_fallback {
                StingerMissingMediaFallback::Cut => "cut",
                StingerMissingMediaFallback::Fade => "fade",
                StingerMissingMediaFallback::KeepProgram => "keep Program",
            }
            .to_owned(),
        ),
    ]
}

/// Draws the slot/media pickers with Assign and Clear keys.
#[allow(clippy::too_many_lines)] // one immediate-mode card
pub(crate) fn draw_stinger_configuration(
    ui: &mut Ui,
    state: &StudioUiState,
    editor: &mut StingerEditor,
    duration_frames: u32,
    intents: &mut Vec<StudioIntent>,
) {
    ui.add_space(8.0);
    let reason = unavailable_reason(state, Capability::Transition);
    editor.slot = editor.slot.clamp(1, STINGER_SLOT_COUNT);
    let stingers = state.view.as_ref().map(|view| view.stingers.as_slice());
    let slot_state = stinger_slot_state(stingers, editor.slot);
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            ui.label(section_label(&format!("Configure S{}", editor.slot)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                status_chip(
                    ui,
                    stinger_slot_presentation(slot_state).label,
                    stinger_slot_tone(slot_state),
                    104.0,
                )
                .on_hover_text("Replicated readiness of the selected slot");
            });
        });
        let Some(view) = &state.view else {
            ui.label(
                RichText::new("Stinger state not synchronized")
                    .size(11.0)
                    .color(MUTED),
            )
            .on_hover_text(reason);
            return;
        };
        let media = stinger_editor_media(view, *editor);
        draw_stinger_pickers(ui, view, editor, media);
        let slot =
            WireStingerSlotId::new(editor.slot).expect("the editor clamps slots to 1 through 8");
        let assign =
            media.and_then(|media| stinger_assign_intent(state, slot, media, duration_frames));
        let remove = stinger_remove_intent(state, slot);
        let session_ok = stinger_configuration_available(state);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let width = ((ui.available_width() - 6.0) / 2.0).floor().max(80.0);
            let key = |label: &str, primary: bool| {
                Button::new(RichText::new(label).size(11.0).strong().color(TEXT))
                    .fill(if primary { ACCENT_FILL } else { WIDGET })
                    .stroke(Stroke::new(
                        1.0,
                        if primary {
                            ACCENT.gamma_multiply(0.6)
                        } else {
                            BORDER_STRONG
                        },
                    ))
                    .min_size(Vec2::new(width, 24.0))
            };
            let assign_text = media.map_or_else(
                || format!("Assign media to Stinger slot {}", editor.slot),
                |media| {
                    format!(
                        "Assign \"{}\" as Stinger slot {} media (other slot settings are kept)",
                        input_name(view, media),
                        editor.slot
                    )
                },
            );
            if ui
                .add_enabled(assign.is_some(), key("ASSIGN", true))
                .hint(
                    &assign_text,
                    if session_ok {
                        "choose a media input"
                    } else {
                        reason
                    },
                )
                .clicked()
                && let Some(intent) = assign.clone()
            {
                intents.push(intent);
            }
            if ui
                .add_enabled(remove.is_some(), key("CLEAR", false))
                .hint(
                    &format!("Clear Stinger slot {}", editor.slot),
                    if session_ok {
                        "this Stinger slot is not configured"
                    } else {
                        reason
                    },
                )
                .clicked()
                && let Some(intent) = remove.clone()
            {
                intents.push(intent);
            }
        });
        egui::Grid::new("studio-stinger-details")
            .num_columns(2)
            .spacing(Vec2::new(12.0, 3.0))
            .show(ui, |ui| {
                for (label, value) in stinger_details(view, editor.slot, duration_frames) {
                    ui.label(RichText::new(label).size(10.5).color(MUTED));
                    ui.label(RichText::new(value).size(10.5).color(TEXT_DIM));
                    ui.end_row();
                }
            })
            .response
            .on_hover_text(stinger_summary(view, editor.slot));
    });
}
