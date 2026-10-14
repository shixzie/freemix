use egui::{Button, CornerRadius, DragValue, Rect, RichText, Sense, Stroke, Ui, Vec2};
use fm_protocol::{FadeToBlackPosition, FadeToBlackState};

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg, with_key};
use crate::theme::{
    AMBER, PROGRAM, border_color, section_label, text_color, text_dim_color, widget_color,
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
                "FTB  ON",
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
            ui.add_enabled(
                availability.duration,
                DragValue::new(duration_frames)
                    .range(1..=3_600)
                    .suffix(" fr")
                    .speed(1.0),
            )
            .hint(l.text(Msg::FtbDurationHover), reason);
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
