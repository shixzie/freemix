use egui::{Button, CornerRadius, DragValue, Rect, RichText, Sense, Stroke, Ui, Vec2};
use fm_protocol::{FadeToBlackPosition, FadeToBlackState};

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg, with_key};
use crate::theme::{
    AMBER, PROGRAM, border_color, field_label, section_label, text_color, text_dim_color,
    widget_color,
};
use crate::{StudioConnectionStatus, StudioIntent, StudioUiState};

/// Pure Fade-to-Black control availability derived from replicated state and session gates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FadeToBlackAvailability {
    pub to_black: bool,
    pub to_live: bool,
    pub duration: bool,
}

/// Session and replicated-state gates for Fade-to-Black controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FadeToBlackGate {
    pub connection_status: StudioConnectionStatus,
    pub has_view: bool,
    pub can_transition: bool,
}

impl FadeToBlackGate {
    #[must_use]
    pub const fn from_state(state: &StudioUiState) -> Self {
        Self {
            connection_status: state.connection_status,
            has_view: state.view.is_some(),
            can_transition: state.can_transition,
        }
    }
}

/// Computes Fade-to-Black availability without drawing or dispatching intents.
#[must_use]
pub const fn fade_to_black_availability(
    gate: FadeToBlackGate,
    desired: Option<FadeToBlackState>,
) -> FadeToBlackAvailability {
    let base = gate.connection_status.controls_enabled() && gate.has_view && gate.can_transition;
    match desired {
        Some(desired) if base => FadeToBlackAvailability {
            to_black: !desired.target_active,
            to_live: desired.target_active,
            duration: true,
        },
        Some(_) | None => FadeToBlackAvailability {
            to_black: false,
            to_live: false,
            duration: false,
        },
    }
}

#[allow(clippy::too_many_lines)] // one immediate-mode region
pub(super) fn draw_fade_to_black(
    ui: &mut Ui,
    duration_frames: &mut u32,
    state: &StudioUiState,
    intents: &mut Vec<StudioIntent>,
) {
    let desired = state
        .view
        .as_ref()
        .map(|view| view.switcher.desired_fade_to_black);
    let realized = state
        .view
        .as_ref()
        .map(|view| view.switcher.realized_fade_to_black);
    let availability = fade_to_black_availability(FadeToBlackGate::from_state(state), desired);
    let black = desired.is_some_and(|desired| desired.target_active);
    let reason = unavailable_reason(state, Capability::Transition);
    let l = l10n::locale(ui.ctx());

    ui.horizontal(|ui| {
        // One console key: it offers exactly the reversal the replicated
        // desired target permits (to black while live, to live while black).
        let (label, active, enabled, hover) = if black {
            (
                l.text(Msg::FtbOnKey),
                false,
                availability.to_live,
                with_key(l, l.text(Msg::FtbReverseHover), "B"),
            )
        } else {
            (
                "FTB",
                true,
                availability.to_black,
                with_key(l, l.text(Msg::FtbHover), "B"),
            )
        };
        // Lit red only while Program is (going) black, as on a hardware
        // panel; an idle FTB key is an unlit console key.
        let (fill, stroke) = if black {
            (PROGRAM, PROGRAM)
        } else {
            (widget_color(), PROGRAM.gamma_multiply(0.45))
        };
        if ui
            .add_enabled(
                enabled,
                Button::new(RichText::new(label).strong().size(13.0).color(text_color()))
                    .fill(fill)
                    .stroke(Stroke::new(crate::theme::line_width(), stroke))
                    .min_size(Vec2::new(100.0, 40.0)),
            )
            .hint(&hover, reason)
            .clicked()
        {
            intents.push(StudioIntent::FadeToBlack {
                active,
                duration_frames: *duration_frames,
            });
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(section_label(l.text(Msg::FadeToBlack)));
            // Duration and outputs share one row, so the block is as tall
            // as the FTB key beside it; a narrow column wraps them.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 2.0);
                ui.add_enabled(
                    availability.duration,
                    DragValue::new(duration_frames)
                        .range(1..=3_600)
                        .suffix(" fr")
                        .speed(1.0),
                )
                .hint(l.text(Msg::FtbDurationHover), reason);
                draw_fade_to_black_outputs(ui, state, intents);
            });
        });
    });
    let phase = fade_to_black_phase(l, desired, realized);
    let color = match phase.tone {
        PhaseTone::Quiet => text_dim_color(),
        PhaseTone::Black => PROGRAM,
        PhaseTone::Pending => AMBER,
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (lamp, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), Sense::hover());
        ui.painter().circle_filled(lamp.center(), 3.5, color);
        // Long Spanish states end in an ellipsis; the tooltip repeats them.
        ui.add(
            egui::Label::new(
                RichText::new(&phase.text)
                    .size(crate::theme::legible(10.5))
                    .strong()
                    .extra_letter_spacing(0.6)
                    .color(color),
            )
            .truncate(),
        );
    })
    .response
    .on_hover_text(format!(
        "{}\n{}",
        phase.text,
        l.format(
            Msg::FtbStateHover,
            &[
                ("desired", Arg::Str(&fade_to_black_label(l, desired))),
                ("realized", Arg::Str(&fade_to_black_label(l, realized))),
            ],
        )
    ));
    // The track's space is always reserved so toggling Fade-to-Black never
    // shifts the layout; it is painted only while Program is not fully live.
    let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 3.0), Sense::hover());
    if let Some(black) = phase.black_percent {
        let painter = ui.painter();
        painter.rect_filled(bar, CornerRadius::same(1), border_color());
        painter.rect_filled(
            Rect::from_min_size(
                bar.min,
                Vec2::new(bar.width() * f32::from(black) / 100.0, bar.height()),
            ),
            CornerRadius::same(1),
            color,
        );
    }
}

/// The outputs Fade to Black fades, as a menu of one checkbox per project
/// output (SW-004): an unticked output stays live while the others fade.
/// Drawn beside the FTB key and in the Output menu; nothing without
/// outputs.
pub(crate) fn draw_fade_to_black_outputs(
    ui: &mut Ui,
    state: &StudioUiState,
    intents: &mut Vec<StudioIntent>,
) {
    let Some(view) = &state.view else {
        return;
    };
    if view.outputs.is_empty() {
        return;
    }
    let l = l10n::locale(ui.ctx());
    let enabled = state.connection_status.controls_enabled() && state.can_transition;
    let reason = unavailable_reason(state, Capability::Transition);
    let affected = view
        .outputs
        .iter()
        .filter(|output| view.fade_to_black_affects(output.output))
        .count();
    #[allow(clippy::cast_possible_truncation)] // at most 256 outputs
    let label = l.format(
        Msg::FtbOutputs,
        &[
            ("affected", Arg::from(affected as u32)),
            ("total", Arg::from(view.outputs.len() as u32)),
        ],
    );
    ui.menu_button(label, |ui| {
        for output in &view.outputs {
            let mut fades = view.fade_to_black_affects(output.output);
            let hover = l.format(
                if fades {
                    Msg::FtbOutputFadesHover
                } else {
                    Msg::FtbOutputLiveHover
                },
                &[("output", Arg::Str(&output.name))],
            );
            if ui
                .add_enabled(
                    enabled,
                    egui::Checkbox::new(&mut fades, output.name.as_str()),
                )
                .hint(&hover, reason)
                .changed()
            {
                intents.push(StudioIntent::SetFadeToBlackOutput {
                    output: output.output,
                    affected: fades,
                });
            }
            draw_output_duration(
                ui,
                (output.output, &output.name),
                view.fade_to_black_output_duration(output.output),
                (enabled, reason),
                intents,
            );
        }
        ui.separator();
        draw_curve(ui, view.fade_to_black_curve, (enabled, reason), intents);
    })
    .response
    .hint(l.text(Msg::FtbOutputsHover), reason);
}

/// One output's `Own duration` toggle and, while it is on, the frames it
/// fades over instead of the shared Fade-to-Black duration (SW-004). A
/// drag sends one edit when released; a typed value when it changes.
fn draw_output_duration(
    ui: &mut Ui,
    (output, name): (fm_types::OutputId, &str),
    duration: Option<u32>,
    (enabled, reason): (bool, &str),
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let output_arg = || [("output", Arg::Str(name))];
    ui.horizontal(|ui| {
        ui.add_space(18.0);
        let mut own = duration.is_some();
        let toggle_hover = l.format(
            if own {
                Msg::FtbOwnDurationOnHover
            } else {
                Msg::FtbOwnDurationOffHover
            },
            &output_arg(),
        );
        if ui
            .add_enabled(
                enabled,
                egui::Checkbox::new(&mut own, l.text(Msg::FtbOwnDuration)),
            )
            .hint(&toggle_hover, reason)
            .changed()
        {
            intents.push(StudioIntent::SetFadeToBlackOutputDuration {
                output,
                duration_frames: own
                    .then_some(crate::StudioShell::DEFAULT_TRANSITION_DURATION_FRAMES),
            });
        }
        let Some(confirmed) = duration else {
            return;
        };
        let draft_id = egui::Id::new(("studio-ftb-output-duration", output));
        let mut frames = ui
            .data(|data| data.get_temp::<u32>(draft_id))
            .unwrap_or(confirmed);
        let response = ui
            .add_enabled(
                enabled,
                DragValue::new(&mut frames)
                    .range(1..=fm_protocol::MAX_FADE_TO_BLACK_DURATION_FRAMES)
                    .suffix(" fr")
                    .speed(1.0),
            )
            .hint(
                &l.format(Msg::FtbOutputDurationHover, &output_arg()),
                reason,
            );
        if response.dragged() {
            ui.data_mut(|data| data.insert_temp(draft_id, frames));
        } else {
            ui.data_mut(|data| data.remove::<u32>(draft_id));
            if (response.drag_stopped() || response.changed()) && frames != confirmed {
                intents.push(StudioIntent::SetFadeToBlackOutputDuration {
                    output,
                    duration_frames: Some(frames),
                });
            }
        }
    });
}

/// The Fade-to-Black curve: Linear or Ease (SW-004).
fn draw_curve(
    ui: &mut Ui,
    current: fm_protocol::FadeToBlackCurve,
    (enabled, reason): (bool, &str),
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(field_label(l.text(Msg::FtbCurveLabel)));
        for curve in fm_protocol::FadeToBlackCurve::ALL {
            let (label, hover) = match curve {
                fm_protocol::FadeToBlackCurve::Linear => {
                    (Msg::AnimEaseLinear, Msg::FtbCurveLinearHover)
                }
                fm_protocol::FadeToBlackCurve::Ease => (Msg::FtbCurveEase, Msg::FtbCurveEaseHover),
            };
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(curve == current, l.text(label)),
                )
                .hint(l.text(hover), reason)
                .clicked()
                && curve != current
            {
                intents.push(StudioIntent::SetFadeToBlackCurve { curve });
            }
        }
    });
}

/// How prominently the Fade-to-Black status line is drawn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PhaseTone {
    /// Program is live and settled.
    Quiet,
    /// Program is (partly) black on air.
    Black,
    /// The desired target is not yet reflected in the realized state.
    Pending,
}

/// One-line operator summary of the realized Fade-to-Black state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FadeToBlackPhase {
    pub(crate) text: String,
    pub(crate) tone: PhaseTone,
    /// Realized black level in whole percent while not fully live.
    pub(crate) black_percent: Option<u8>,
}

/// Summarizes the realized (on-air) Fade-to-Black state, flagging a desired
/// target the daemon has not yet adopted.
pub(crate) fn fade_to_black_phase(
    l: Locale,
    desired: Option<FadeToBlackState>,
    realized: Option<FadeToBlackState>,
) -> FadeToBlackPhase {
    let Some(realized) = realized else {
        return FadeToBlackPhase {
            text: l.text(Msg::FtbStateUnavailable).to_owned(),
            tone: PhaseTone::Quiet,
            black_percent: None,
        };
    };
    let numerator = u32::from(realized.position.numerator());
    #[allow(clippy::cast_possible_truncation)] // at most 100
    let percent = (numerator * 100 / FadeToBlackPosition::DENOMINATOR) as u8;
    let pending = desired.is_some_and(|desired| desired.target_active != realized.target_active);
    let percent_text = l.percent(i64::from(percent));
    let percent_arg = [("percent", Arg::Str(&percent_text))];
    let (text, tone) = match (realized.target_active, numerator) {
        (false, 0) => (l.text(Msg::FtbProgramLive).to_owned(), PhaseTone::Quiet),
        (true, n) if n == FadeToBlackPosition::DENOMINATOR => {
            (l.text(Msg::FtbProgramBlack).to_owned(), PhaseTone::Black)
        }
        (true, _) => (l.format(Msg::FtbFading, &percent_arg), PhaseTone::Black),
        (false, _) => (l.format(Msg::FtbRestoring, &percent_arg), PhaseTone::Black),
    };
    if pending {
        let requested = if desired.is_some_and(|desired| desired.target_active) {
            Msg::FtbBlackRequested
        } else {
            Msg::FtbLiveRequested
        };
        return FadeToBlackPhase {
            text: l.format(requested, &[("state", Arg::Str(&text))]),
            tone: PhaseTone::Pending,
            black_percent: (numerator > 0).then_some(percent),
        };
    }
    FadeToBlackPhase {
        text,
        tone,
        black_percent: (numerator > 0).then_some(percent),
    }
}

fn fade_to_black_label(l: Locale, state: Option<FadeToBlackState>) -> String {
    state.map_or_else(
        || l.text(Msg::StreamUnavailable).to_owned(),
        |state| {
            l.format(
                Msg::FtbTarget,
                &[
                    (
                        "target",
                        Arg::Str(l.text(if state.target_active {
                            Msg::FtbWordBlack
                        } else {
                            Msg::Live
                        })),
                    ),
                    (
                        "position",
                        Arg::Str(&format!(
                            "{}/{}",
                            state.position.numerator(),
                            FadeToBlackPosition::DENOMINATOR
                        )),
                    ),
                ],
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVAILABLE: FadeToBlackGate = FadeToBlackGate {
        connection_status: StudioConnectionStatus::Ready,
        has_view: true,
        can_transition: true,
    };

    fn state(target_active: bool, position: u16) -> FadeToBlackState {
        FadeToBlackState {
            target_active,
            position: FadeToBlackPosition::new(position),
        }
    }

    #[test]
    fn availability_enables_only_the_opposite_target_for_reversal() {
        assert_eq!(
            fade_to_black_availability(AVAILABLE, Some(state(false, 0))),
            FadeToBlackAvailability {
                to_black: true,
                to_live: false,
                duration: true,
            }
        );
        assert_eq!(
            fade_to_black_availability(AVAILABLE, Some(state(true, 30_000))),
            FadeToBlackAvailability {
                to_black: false,
                to_live: true,
                duration: true,
            }
        );
    }

    #[test]
    fn every_session_and_state_gate_disables_controls() {
        for gate in [
            FadeToBlackGate {
                connection_status: StudioConnectionStatus::Connecting,
                ..AVAILABLE
            },
            FadeToBlackGate {
                has_view: false,
                ..AVAILABLE
            },
            FadeToBlackGate {
                can_transition: false,
                ..AVAILABLE
            },
        ] {
            assert_eq!(
                fade_to_black_availability(gate, Some(state(false, 0))),
                FadeToBlackAvailability {
                    to_black: false,
                    to_live: false,
                    duration: false,
                }
            );
        }
        assert_eq!(
            fade_to_black_availability(AVAILABLE, None),
            FadeToBlackAvailability {
                to_black: false,
                to_live: false,
                duration: false,
            }
        );
    }

    #[test]
    fn exact_desired_and_realized_labels_are_stable() {
        assert_eq!(
            fade_to_black_label(Locale::ENGLISH, Some(state(true, 40_000))),
            "TARGET BLACK @ 40000/65535"
        );
        assert_eq!(
            fade_to_black_label(Locale::ENGLISH, Some(state(false, 20_000))),
            "TARGET LIVE @ 20000/65535"
        );
        assert_eq!(fade_to_black_label(Locale::ENGLISH, None), "UNAVAILABLE");
    }

    #[test]
    fn phase_reports_the_realized_on_air_state_and_pending_targets() {
        let live = state(false, 0);
        let black = state(true, 65_535);
        assert_eq!(
            fade_to_black_phase(Locale::ENGLISH, Some(live), Some(live)),
            FadeToBlackPhase {
                text: "PROGRAM LIVE".to_owned(),
                tone: PhaseTone::Quiet,
                black_percent: None,
            }
        );
        assert_eq!(
            fade_to_black_phase(Locale::ENGLISH, Some(black), Some(black)),
            FadeToBlackPhase {
                text: "PROGRAM BLACK".to_owned(),
                tone: PhaseTone::Black,
                black_percent: Some(100),
            }
        );
        let fading = fade_to_black_phase(Locale::ENGLISH, Some(black), Some(state(true, 32_768)));
        assert_eq!(fading.text, "FADING TO BLACK 50%");
        assert_eq!(fading.black_percent, Some(50));
        let restoring =
            fade_to_black_phase(Locale::ENGLISH, Some(live), Some(state(false, 16_384)));
        assert_eq!(restoring.text, "RESTORING · 25% BLACK");
        assert_eq!(restoring.tone, PhaseTone::Black);
        let requested = fade_to_black_phase(Locale::ENGLISH, Some(black), Some(live));
        assert_eq!(requested.text, "PROGRAM LIVE · BLACK REQUESTED");
        assert_eq!(requested.tone, PhaseTone::Pending);
        assert_eq!(requested.black_percent, None);
        assert_eq!(
            fade_to_black_phase(Locale::ENGLISH, Some(live), None).text,
            "FTB STATE UNAVAILABLE"
        );
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        assert_eq!(
            fade_to_black_phase(spanish, Some(live), Some(state(false, 16_384))).text,
            "RESTAURANDO · 25\u{a0}% NEGRO"
        );
        assert_eq!(
            fade_to_black_phase(spanish, Some(black), Some(live)).text,
            "PROGRAMA EN VIVO · NEGRO PEDIDO"
        );
    }
}
