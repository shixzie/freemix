//! Compact overlay channel cards for the side panel.

use egui::{Button, CornerRadius, Frame, Margin, RichText, Stroke, Ui, Vec2};
use fm_protocol::{
    OverlayBorderPreset, OverlayPositionPreset, OverlayTransitionKind, WireOverlayChannelId,
};
use fm_types::InputId;

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Msg};
use crate::monitors::label_for_input;
use crate::theme::{
    ACCENT, AMBER, BORDER, GRAPHITE_RAISED, MUTED, PROGRAM, TEXT, TEXT_DIM, caption, field_label,
    mono, pill, small_button,
};
use crate::{StudioIntent, StudioUiState, overlay_status};

/// The specific reason when session gates pass, the session reason otherwise.
fn why(enabled: bool, session: &'static str, specific: &'static str) -> &'static str {
    if enabled { specific } else { session }
}

pub(crate) fn draw_overlays(
    ui: &mut Ui,
    state: &StudioUiState,
    duration_frames: u32,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let enabled =
        state.connection_status.controls_enabled() && state.can_transition && state.view.is_some();
    ui.add(egui::Label::new(caption(l.text(Msg::OverlaysIntro))).wrap())
        .on_hover_text(l.text(Msg::OverlaysIntroHover));
    ui.add_space(2.0);
    // Eight rows stay compact so the side panel keeps room for outputs; a
    // row grows only while its details are open.
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = OVERLAY_ROW_GAP;
        for channel_number in 1_u8..=8 {
            draw_overlay_channel(ui, state, channel_number, duration_frames, enabled, intents);
        }
    });
}

/// Vertical gap between overlay rows.
const OVERLAY_ROW_GAP: f32 = 4.0;
/// Height of an overlay row's header controls.
const OVERLAY_ROW_HEIGHT: f32 = 20.0;

/// Operator state of one overlay channel derived from desired and confirmed
/// flags, as catalog English (worded in the console language when drawn).
const fn overlay_state_label(desired_active: bool, confirmed_active: bool) -> (&'static str, bool) {
    match (desired_active, confirmed_active) {
        (true, true) => (Msg::OverlayOnAir.en(), true),
        (true, false) => (Msg::OverlayTaking.en(), false),
        (false, true) => (Msg::OverlayClearing.en(), false),
        (false, false) => (Msg::StreamOff.en(), false),
    }
}

#[allow(clippy::too_many_lines)] // one immediate-mode region
fn draw_overlay_channel(
    ui: &mut Ui,
    state: &StudioUiState,
    channel_number: u8,
    duration_frames: u32,
    enabled: bool,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let channel = WireOverlayChannelId::new(channel_number)
        .expect("Studio renders only overlay channels 1 through 8");
    let (desired, realized) = state.view.as_ref().map_or((None, None), |view| {
        (
            view.desired_overlays
                .iter()
                .find(|overlay| overlay.channel == channel_number),
            view.realized_overlays
                .iter()
                .find(|overlay| overlay.channel == channel_number),
        )
    });
    let source = state
        .view
        .as_ref()
        .map(|view| view.switcher.desired.preview);
    let reason = unavailable_reason(state, Capability::Transition);
    let desired_active = desired.is_some_and(|overlay| overlay.active);
    let confirmed_active = realized.is_some_and(|overlay| overlay.active);
    let synchronized = state.view.is_some();
    let (state_label, on_air) = if synchronized {
        overlay_state_label(desired_active, confirmed_active)
    } else {
        (Msg::StatusNoState.en(), false)
    };
    let state_color = if on_air {
        PROGRAM
    } else if desired_active != confirmed_active {
        AMBER
    } else {
        MUTED
    };
    Frame::new()
        .fill(GRAPHITE_RAISED)
        .stroke(Stroke::new(
            1.0,
            if on_air {
                PROGRAM.gamma_multiply(0.6)
            } else {
                BORDER
            },
        ))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
            ui.spacing_mut().interact_size.y = OVERLAY_ROW_HEIGHT;
            let id = ui.make_persistent_id(("studio-overlay-channel", channel_number));
            egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                desired_active,
            )
            .show_header(ui, |ui| {
                ui.label(
                    RichText::new(format!("O{channel_number}"))
                        .size(12.5)
                        .strong()
                        .color(TEXT),
                );
                pill(ui, l.tr(state_label), state_color, on_air);
                let source_name = desired
                    .and_then(|overlay| overlay.source)
                    .zip(state.view.as_ref())
                    .map(|(source, view)| label_for_input(l, view, source));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            enabled && desired_active,
                            Button::new(
                                RichText::new(l.text(Msg::OverlayOffKey))
                                    .size(11.0)
                                    .strong(),
                            )
                            .min_size(Vec2::new(0.0, OVERLAY_ROW_HEIGHT - 2.0)),
                        )
                        .hint(
                            l.text(Msg::OverlayOffHover),
                            why(enabled, reason, Msg::ReasonOverlayAlreadyOff.en()),
                        )
                        .clicked()
                    {
                        intents.push(StudioIntent::OverlayOff { channel });
                    }
                    if ui
                        .add_enabled(
                            enabled && source.is_some(),
                            small_button(l.text(Msg::TakeKey))
                                .min_size(Vec2::new(0.0, OVERLAY_ROW_HEIGHT - 2.0))
                                .fill(crate::theme::ACCENT_FILL)
                                .stroke(Stroke::new(1.0, ACCENT.gamma_multiply(0.6))),
                        )
                        .hint(
                            l.text(Msg::OverlayTakeHover),
                            why(enabled, reason, Msg::ReasonNoPreviewSource.en()),
                        )
                        .clicked()
                        && let Some(source) = source
                    {
                        intents.push(StudioIntent::TakeOverlay { channel, source });
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(source_name.unwrap_or_else(|| {
                                l.text(if synchronized {
                                    Msg::OverlayNoSource
                                } else {
                                    Msg::RowNotSynchronized
                                })
                                .to_owned()
                            }))
                            .size(11.5)
                            .color(TEXT_DIM),
                        )
                        .truncate(),
                    );
                });
            })
            .body_unindented(|ui| {
                draw_overlay_details(
                    ui,
                    state,
                    OverlayChannelContext {
                        channel,
                        channel_number,
                        desired,
                        realized,
                        source,
                        duration_frames,
                        enabled,
                    },
                    intents,
                );
            });
        });
}

#[derive(Clone, Copy)]
struct OverlayChannelContext<'a> {
    channel: WireOverlayChannelId,
    channel_number: u8,
    desired: Option<&'a fm_ui_model::OverlayStatus>,
    realized: Option<&'a fm_ui_model::OverlayStatus>,
    source: Option<InputId>,
    duration_frames: u32,
    enabled: bool,
}

#[allow(clippy::too_many_lines)] // one immediate-mode region
fn draw_overlay_details(
    ui: &mut Ui,
    state: &StudioUiState,
    context: OverlayChannelContext<'_>,
    intents: &mut Vec<StudioIntent>,
) {
    let OverlayChannelContext {
        channel,
        channel_number,
        desired,
        realized,
        source,
        duration_frames,
        enabled,
    } = context;
    let reason = unavailable_reason(state, Capability::Transition);
    let l = l10n::locale(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        let update = overlay_update_intent(enabled, channel, desired, source);
        if ui
            .add_enabled(update.is_some(), small_button(l.text(Msg::UpdateKey)))
            .hint(
                l.text(Msg::OverlayUpdateHover),
                why(enabled, reason, Msg::ReasonOverlayUpdate.en()),
            )
            .clicked()
            && let Some(update) = update
        {
            intents.push(update);
        }
        if ui
            .add_enabled(
                enabled && source.is_some(),
                small_button(format!(
                    "Q+{}",
                    desired.map_or(0, |value| value.queued_sources.len())
                )),
            )
            .hint(
                l.text(Msg::OverlayQueueHover),
                why(enabled, reason, Msg::ReasonNoPreviewSource.en()),
            )
            .clicked()
            && let Some(source) = source
        {
            intents.push(StudioIntent::QueueOverlay { channel, source });
        }
        if ui
            .add_enabled(
                enabled && desired.is_some_and(|value| !value.queued_sources.is_empty()),
                small_button(l.text(Msg::NextKey)),
            )
            .hint(
                l.text(Msg::OverlayNextHover),
                why(enabled, reason, Msg::ReasonOverlayQueueEmpty.en()),
            )
            .clicked()
        {
            intents.push(StudioIntent::TakeNextOverlay { channel });
        }
        let transition = desired.map_or(OverlayTransitionKind::Cut, |overlay| overlay.transition);
        if ui
            .add_enabled(
                enabled,
                small_button(l.text(match transition {
                    OverlayTransitionKind::Cut => Msg::CutKey,
                    OverlayTransitionKind::Fade => Msg::FadeKey,
                })),
            )
            .hint(l.text(Msg::OverlayTransitionHover), reason)
            .clicked()
        {
            intents.push(StudioIntent::ToggleOverlayTransition {
                channel,
                duration_frames,
            });
        }
        let position = desired.map_or(OverlayPositionPreset::FullFrame, |overlay| overlay.position);
        let border = desired.map_or(OverlayBorderPreset::None, |overlay| overlay.border);
        if ui
            .add_enabled(
                enabled,
                small_button(l.text(overlay_status::position_label(position))),
            )
            .hint(l.text(Msg::OverlayPositionHover), reason)
            .clicked()
        {
            intents.push(StudioIntent::CycleOverlayPosition { channel });
        }
        if ui
            .add_enabled(
                enabled,
                small_button(l.text(overlay_status::border_label(border))),
            )
            .hint(l.text(Msg::OverlayBorderHover), reason)
            .clicked()
        {
            intents.push(StudioIntent::CycleOverlayBorder { channel });
        }
    });
    let outputs = state.view.as_ref().map(|view| view.outputs.as_slice());
    match outputs {
        Some(outputs) if !outputs.is_empty() => {
            ui.horizontal_wrapped(|ui| {
                ui.label(field_label(l.text(Msg::OverlayOutputs)));
                for output in outputs {
                    let mut included = desired
                        .is_some_and(|overlay| overlay.included_outputs.contains(&output.output));
                    if ui
                        .add_enabled(
                            enabled && desired.is_some(),
                            egui::Checkbox::new(
                                &mut included,
                                RichText::new(&output.name).size(11.0),
                            ),
                        )
                        .hint(
                            l.text(Msg::OverlayOutputHover),
                            why(enabled, reason, Msg::ReasonOverlayNotTaken.en()),
                        )
                        .changed()
                    {
                        intents.push(StudioIntent::ToggleOverlayOutput {
                            channel,
                            output: output.output,
                        });
                    }
                }
            });
        }
        Some(_) => {
            ui.label(
                RichText::new(l.text(Msg::OverlayOutputsNone))
                    .size(10.0)
                    .color(MUTED),
            );
        }
        None => {
            ui.label(
                RichText::new(l.text(Msg::OverlayOutputsUnsynced))
                    .size(10.0)
                    .color(MUTED),
            );
        }
    }
    ui.add(
        egui::Label::new(
            mono(overlay_status::format(
                l,
                channel_number,
                desired,
                realized,
                |input| {
                    state.view.as_ref().map_or_else(
                        || crate::monitors::fallback_input_name(l, input),
                        |view| label_for_input(l, view, input),
                    )
                },
            ))
            .size(9.5)
            .color(MUTED),
        )
        .wrap(),
    );
}

pub(crate) fn overlay_update_intent(
    enabled: bool,
    channel: WireOverlayChannelId,
    desired: Option<&fm_ui_model::OverlayStatus>,
    source: Option<InputId>,
) -> Option<StudioIntent> {
    if !enabled || !desired.is_some_and(|overlay| overlay.active) {
        return None;
    }
    source.map(|source| StudioIntent::UpdateOverlay { channel, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_state_label_separates_desired_and_confirmed() {
        assert_eq!(overlay_state_label(true, true), ("ON AIR", true));
        assert_eq!(overlay_state_label(true, false), ("TAKING", false));
        assert_eq!(overlay_state_label(false, true), ("CLEARING", false));
        assert_eq!(overlay_state_label(false, false), ("OFF", false));
    }
}
