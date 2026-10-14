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
use crate::l10n::{self, Arg, Locale, Msg, frames};
use crate::panels::{stinger_slot_presentation, stinger_slot_state, stinger_slot_tone};
use crate::theme::{
    ACCENT_FILL, accent_color, border_strong_color, card, lock_badge, mono, muted_color,
    section_label, status_chip, text_color, text_dim_color, widget_color,
};
use crate::{
    StudioIntent, StudioUiState, TransitionGate, stinger_edit_lock_reason, transition_availability,
};

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

/// Returns why Stinger slot setup is unavailable, if it is: a session gate
/// first, then the project padlock (slot setup is a project edit).
#[must_use]
pub fn stinger_setup_block_reason(state: &StudioUiState) -> Option<&'static str> {
    if stinger_configuration_available(state) {
        stinger_edit_lock_reason(state)
    } else {
        Some(unavailable_reason(state, Capability::Transition))
    }
}

/// Returns the assignment intent when the session allows Stinger configuration,
/// the project is unlocked, and `media_input` is an input of the replicated view.
#[must_use]
pub fn stinger_assign_intent(
    state: &StudioUiState,
    slot: WireStingerSlotId,
    media_input: InputId,
    duration_frames: u32,
) -> Option<StudioIntent> {
    let view = state.view.as_ref()?;
    (stinger_setup_block_reason(state).is_none() && view.inputs.contains(&media_input)).then_some(
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
    (stinger_setup_block_reason(state).is_none()
        && configured_stinger(view, slot.number()).is_some())
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

fn input_name(l: Locale, view: &ClientView, input: InputId) -> String {
    crate::monitors::label_for_input(l, view, input)
}

/// The audio policy in words.
const fn audio_policy_text(policy: StingerAudioPolicy) -> Msg {
    match policy {
        StingerAudioPolicy::Muted => Msg::StingerAudioMuted,
        StingerAudioPolicy::StingerOnly => Msg::StingerAudioOnly,
        StingerAudioPolicy::MixWithProgram => Msg::StingerAudioMixed,
    }
}

/// The missing-media fallback in words.
const fn fallback_text(fallback: StingerMissingMediaFallback) -> Msg {
    match fallback {
        StingerMissingMediaFallback::Cut => Msg::StingerFallbackCut,
        StingerMissingMediaFallback::Fade => Msg::StingerFallbackFade,
        StingerMissingMediaFallback::KeepProgram => Msg::StingerFallbackKeep,
    }
}

const fn yes_no(value: bool) -> Msg {
    if value { Msg::WordYes } else { Msg::WordNo }
}

/// One-line summary of a slot's replicated configuration.
pub(crate) fn stinger_summary(l: Locale, view: &ClientView, slot: u8) -> String {
    let slot_arg = ("slot", Arg::Plain(u64::from(slot)));
    let Some(status) = configured_stinger(view, slot) else {
        return l.format(Msg::StingerNotConfigured, &[slot_arg]);
    };
    l.format(
        Msg::StingerSummary,
        &[
            slot_arg,
            ("media", Arg::Str(&input_name(l, view, status.media_input))),
            ("cut", Arg::Str(&frames(l, status.cut_point_frames))),
            ("preload", Arg::Str(l.text(yes_no(status.preload)))),
            (
                "audio",
                Arg::Str(l.text(audio_policy_text(status.audio_policy))),
            ),
            (
                "fallback",
                Arg::Str(l.text(fallback_text(status.missing_media_fallback))),
            ),
        ],
    )
}

/// The slot and media pickers of the configuration row.
fn draw_stinger_pickers(
    ui: &mut Ui,
    view: &ClientView,
    editor: &mut StingerEditor,
    media: Option<InputId>,
) {
    let l = l10n::locale(ui.ctx());
    let stingers = Some(view.stingers.as_slice());
    let slot_text = |slot: u8| {
        l.format(
            Msg::StingerSlotPick,
            &[
                ("slot", Arg::Plain(u64::from(slot))),
                (
                    "state",
                    Arg::Str(
                        l.tr(stinger_slot_presentation(stinger_slot_state(stingers, slot)).label),
                    ),
                ),
            ],
        )
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        // Half the row each, so long translated states and names truncate
        // inside their pickers rather than pushing the media picker out.
        let half = ((ui.available_width() - 6.0) / 2.0).floor().max(100.0);
        ComboBox::from_id_salt("studio-stinger-config-slot")
            .width(half)
            .truncate()
            .selected_text(mono(slot_text(editor.slot)).size(10.5))
            .show_ui(ui, |ui| {
                for slot in 1..=STINGER_SLOT_COUNT {
                    if ui
                        .selectable_label(editor.slot == slot, slot_text(slot))
                        .clicked()
                    {
                        editor.slot = slot;
                        editor.media = None;
                    }
                }
            })
            .response
            .on_hover_text(l.text(Msg::StingerSlotPickHover));
        ComboBox::from_id_salt("studio-stinger-config-media")
            .width(ui.available_width() - 8.0)
            .truncate()
            .selected_text(
                RichText::new(media.map_or_else(
                    || l.text(Msg::StingerNoInput).to_owned(),
                    |m| input_name(l, view, m),
                ))
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
            .on_hover_text(l.text(Msg::StingerMediaPickHover));
    });
}

/// Labelled facts about one slot for the Configure section: the replicated
/// settings of a configured slot, or the defaults ASSIGN applies to an empty one.
pub(crate) fn stinger_details(
    l: Locale,
    view: &ClientView,
    slot: u8,
    duration_frames: u32,
) -> [(&'static str, String); 4] {
    let labels = [
        l.text(Msg::StingerCutPoint),
        l.text(Msg::StingerPreload),
        l.text(Msg::StingerAudio),
        l.text(Msg::StingerFallback),
    ];
    let (values, defaults) = match configured_stinger(view, slot) {
        Some(status) => (
            [
                frames(l, status.cut_point_frames),
                l.text(yes_no(status.preload)).to_owned(),
                l.text(audio_policy_text(status.audio_policy)).to_owned(),
                l.text(fallback_text(status.missing_media_fallback))
                    .to_owned(),
            ],
            false,
        ),
        // An empty slot lists exactly what ASSIGN would configure.
        None => (
            [
                frames(l, default_stinger_cut_point_frames(duration_frames)),
                l.text(Msg::WordYes).to_owned(),
                l.text(Msg::StingerAudioMixed).to_owned(),
                l.text(Msg::StingerFallbackCut).to_owned(),
            ],
            true,
        ),
    };
    let mut values = values.into_iter().map(|value| {
        if defaults {
            l.format(Msg::WithDefault, &[("value", Arg::Str(&value))])
        } else {
            value
        }
    });
    labels.map(|label| (label, values.next().unwrap_or_default()))
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
    let blocked = stinger_setup_block_reason(state);
    let reason = blocked.unwrap_or_else(|| unavailable_reason(state, Capability::Transition));
    let locked = blocked.is_some() && stinger_edit_lock_reason(state) == blocked;
    editor.slot = editor.slot.clamp(1, STINGER_SLOT_COUNT);
    let stingers = state.view.as_ref().map(|view| view.stingers.as_slice());
    let slot_state = stinger_slot_state(stingers, editor.slot);
    let l = l10n::locale(ui.ctx());
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            ui.label(section_label(&l.format(
                Msg::StingerConfigure,
                &[("slot", Arg::Plain(u64::from(editor.slot)))],
            )));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = stinger_slot_presentation(slot_state).label;
                // Sized to the translated word so `ESTADO NO DISPONIBLE`
                // fits as well as `READY`.
                let width = ui
                    .painter()
                    .layout_no_wrap(
                        l.tr(label).to_owned(),
                        egui::FontId::new(9.5, egui::FontFamily::Proportional),
                        text_color(),
                    )
                    .size()
                    .x
                    + 16.0;
                status_chip(ui, label, stinger_slot_tone(slot_state), width.max(72.0))
                    .on_hover_text(l.text(Msg::StingerReadinessHover));
                if locked {
                    lock_badge(ui).on_hover_text(l.text(Msg::StingerLockedHover));
                }
            });
        });
        let Some(view) = &state.view else {
            ui.label(
                RichText::new(l.text(Msg::StingerConfigNotSynced))
                    .size(11.0)
                    .color(muted_color()),
            )
            .on_hover_text(l10n::unavailable(l, reason));
            return;
        };
        let media = stinger_editor_media(view, *editor);
        draw_stinger_pickers(ui, view, editor, media);
        let slot =
            WireStingerSlotId::new(editor.slot).expect("the editor clamps slots to 1 through 8");
        let assign =
            media.and_then(|media| stinger_assign_intent(state, slot, media, duration_frames));
        let remove = stinger_remove_intent(state, slot);
        let session_ok = blocked.is_none();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let width = ((ui.available_width() - 6.0) / 2.0).floor().max(80.0);
            let key = |label: &str, primary: bool| {
                Button::new(RichText::new(label).size(11.0).strong().color(text_color()))
                    .fill(if primary { ACCENT_FILL } else { widget_color() })
                    .stroke(Stroke::new(
                        1.0,
                        if primary {
                            accent_color().gamma_multiply(0.6)
                        } else {
                            border_strong_color()
                        },
                    ))
                    .min_size(Vec2::new(width, 24.0))
            };
            let slot_arg = ("slot", Arg::Plain(u64::from(editor.slot)));
            let assign_text = media.map_or_else(
                || l.format(Msg::StingerAssignHover, &[slot_arg]),
                |media| {
                    l.format(
                        Msg::StingerAssignNamedHover,
                        &[slot_arg, ("name", Arg::Str(&input_name(l, view, media)))],
                    )
                },
            );
            if ui
                .add_enabled(assign.is_some(), key(l.text(Msg::AssignKey), true))
                .hint(
                    &assign_text,
                    if session_ok {
                        Msg::ReasonChooseMedia.en()
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
                .add_enabled(remove.is_some(), key(l.text(Msg::ClearKey), false))
                .hint(
                    &l.format(Msg::ClearStingerSlotHover, &[slot_arg]),
                    if session_ok {
                        Msg::ReasonStingerSlotNotConfigured.en()
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
                for (label, value) in stinger_details(l, view, editor.slot, duration_frames) {
                    ui.label(RichText::new(label).size(10.5).color(muted_color()));
                    ui.label(RichText::new(value).size(10.5).color(text_dim_color()));
                    ui.end_row();
                }
            })
            .response
            .on_hover_text(stinger_summary(l, view, editor.slot));
    });
}
