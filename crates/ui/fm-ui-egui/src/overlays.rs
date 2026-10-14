//! Compact overlay channel cards for the side panel.

use egui::{Button, CornerRadius, Frame, Margin, RichText, Stroke, Ui, Vec2};
use fm_protocol::{
    OverlayBorderPreset, OverlayPositionPreset, OverlayTransitionKind, WireOverlayChannelId,
};
use fm_types::InputId;

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::monitors::label_for_input;
use crate::theme::{
    ACCENT, AMBER, BORDER, GRAPHITE_RAISED, MUTED, PROGRAM, TEXT, TEXT_DIM, mono, pill,
    small_button,
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
    let enabled =
        state.connection_status.controls_enabled() && state.can_transition && state.view.is_some();
    ui.label(
        RichText::new(
            "TAKE uses the current Preview source. D = desired, C = last daemon confirmation.",
        )
        .size(10.5)
        .color(MUTED),
    )
    .on_hover_text("Confirmation is control state, not output-video proof.");
    ui.add_space(2.0);
    for channel_number in 1_u8..=8 {
        draw_overlay_channel(ui, state, channel_number, duration_frames, enabled, intents);
    }
}

/// Operator state of one overlay channel derived from desired and confirmed flags.
const fn overlay_state_label(desired_active: bool, confirmed_active: bool) -> (&'static str, bool) {
    match (desired_active, confirmed_active) {
        (true, true) => ("ON AIR", true),
        (true, false) => ("TAKING", false),
        (false, true) => ("CLEARING", false),
        (false, false) => ("OFF", false),
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
        ("NO STATE", false)
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
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::symmetric(8, 6))
        .outer_margin(Margin {
            left: 0,
            right: 0,
            top: 0,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
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
                pill(ui, state_label, state_color, on_air);
                let source_name = desired
                    .and_then(|overlay| overlay.source)
                    .zip(state.view.as_ref())
                    .map(|(source, view)| label_for_input(view, source));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            enabled && desired_active,
                            Button::new(RichText::new("OFF").size(11.0).strong())
                                .min_size(Vec2::new(0.0, 20.0)),
                        )
                        .hint(
                            "Remove this overlay channel from Program",
                            why(enabled, reason, "this overlay channel is already off"),
                        )
                        .clicked()
                    {
                        intents.push(StudioIntent::OverlayOff { channel });
                    }
                    if ui
                        .add_enabled(
                            enabled && source.is_some(),
                            small_button("TAKE")
                                .fill(crate::theme::ACCENT_FILL)
                                .stroke(Stroke::new(1.0, ACCENT.gamma_multiply(0.6))),
                        )
                        .hint(
                            "Take the current Preview source on this overlay channel",
                            why(enabled, reason, "no Preview source is selected"),
                        )
                        .clicked()
                        && let Some(source) = source
                    {
                        intents.push(StudioIntent::TakeOverlay { channel, source });
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(source_name.unwrap_or_else(|| {
                                if synchronized {
                                    "no source".to_owned()
                                } else {
                                    "not synchronized".to_owned()
                                }
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
    ui.horizontal_wrapped(|ui| {
        let update = overlay_update_intent(enabled, channel, desired, source);
        if ui
            .add_enabled(update.is_some(), small_button("UPDATE"))
            .hint(
                "Update this active overlay channel from the current Preview source",
                why(
                    enabled,
                    reason,
                    "the channel is off or already shows the Preview source",
                ),
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
                "Append the current Preview source to this overlay queue",
                why(enabled, reason, "no Preview source is selected"),
            )
            .clicked()
            && let Some(source) = source
        {
            intents.push(StudioIntent::QueueOverlay { channel, source });
        }
        if ui
            .add_enabled(
                enabled && desired.is_some_and(|value| !value.queued_sources.is_empty()),
                small_button("NEXT"),
            )
            .hint(
                "Take and remove the head of this overlay queue",
                why(enabled, reason, "this overlay queue is empty"),
            )
            .clicked()
        {
            intents.push(StudioIntent::TakeNextOverlay { channel });
        }
        let transition = desired.map_or(OverlayTransitionKind::Cut, |overlay| overlay.transition);
        if ui
            .add_enabled(
                enabled,
                small_button(match transition {
                    OverlayTransitionKind::Cut => "CUT",
                    OverlayTransitionKind::Fade => "FADE",
                }),
            )
            .hint("Toggle this overlay channel between Cut and Fade", reason)
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
                small_button(overlay_status::position_label(position)),
            )
            .hint("Cycle this overlay channel's position preset", reason)
            .clicked()
        {
            intents.push(StudioIntent::CycleOverlayPosition { channel });
        }
        if ui
            .add_enabled(enabled, small_button(overlay_status::border_label(border)))
            .hint("Cycle this overlay channel's white border preset", reason)
            .clicked()
        {
            intents.push(StudioIntent::CycleOverlayBorder { channel });
        }
    });
    let outputs = state.view.as_ref().map(|view| view.outputs.as_slice());
    match outputs {
        Some(outputs) if !outputs.is_empty() => {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("OUTPUTS").size(10.0).color(MUTED));
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
                            "Include this overlay channel on this output",
                            why(
                                enabled,
                                reason,
                                "the overlay channel has not been taken yet",
                            ),
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
                RichText::new("OUTPUTS unavailable: no configured outputs")
                    .size(10.0)
                    .color(MUTED),
            );
        }
        None => {
            ui.label(
                RichText::new("OUTPUTS unavailable: project state not synchronized")
                    .size(10.0)
                    .color(MUTED),
            );
        }
    }
    ui.add(
        egui::Label::new(
            mono(overlay_status::format(channel_number, desired, realized))
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
