use egui::{Button, DragValue, RichText, Stroke, Ui, Vec2};
use fm_protocol::{FadeToBlackPosition, FadeToBlackState};

use crate::theme::{PROGRAM, PROGRAM_FILL, TEXT, TEXT_DIM, mono, section_label};
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

    ui.horizontal(|ui| {
        // One console key: it offers exactly the reversal the replicated
        // desired target permits (to black while live, to live while black).
        let (label, active, enabled, hover) = if black {
            (
                "FTB  ON",
                false,
                availability.to_live,
                "Reverse Fade-to-Black and restore live Program  [B]",
            )
        } else {
            (
                "FTB",
                true,
                availability.to_black,
                "Fade the realized Program video and audio to black  [B]",
            )
        };
        let (fill, stroke) = if black {
            (PROGRAM, PROGRAM)
        } else {
            (PROGRAM_FILL, PROGRAM.gamma_multiply(0.7))
        };
        if ui
            .add_enabled(
                enabled,
                Button::new(RichText::new(label).strong().size(13.0).color(TEXT))
                    .fill(fill)
                    .stroke(Stroke::new(1.0, stroke))
                    .min_size(Vec2::new(100.0, 40.0)),
            )
            .on_hover_text(hover)
            .clicked()
        {
            intents.push(StudioIntent::FadeToBlack {
                active,
                duration_frames: *duration_frames,
            });
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(section_label("Fade to black"));
            ui.add_enabled(
                availability.duration,
                DragValue::new(duration_frames)
                    .range(1..=3_600)
                    .suffix(" fr")
                    .speed(1.0),
            )
            .on_hover_text("Fade-to-Black duration in frames");
        });
    });
    ui.add(
        egui::Label::new(
            mono(format!(
                "D {} · R {}",
                fade_to_black_short_label(desired),
                fade_to_black_short_label(realized)
            ))
            .size(9.5)
            .color(TEXT_DIM),
        )
        .wrap(),
    )
    .on_hover_text(format!(
        "DESIRED {}\nREALIZED {}",
        fade_to_black_label(desired),
        fade_to_black_label(realized)
    ));
}

/// Compact target-and-percent label for the transition column.
fn fade_to_black_short_label(state: Option<FadeToBlackState>) -> String {
    state.map_or_else(
        || "UNAVAILABLE".to_owned(),
        |state| {
            format!(
                "{} {}%",
                if state.target_active { "BLACK" } else { "LIVE" },
                u32::from(state.position.numerator()) * 100 / FadeToBlackPosition::DENOMINATOR,
            )
        },
    )
}

fn fade_to_black_label(state: Option<FadeToBlackState>) -> String {
    state.map_or_else(
        || "UNAVAILABLE".to_owned(),
        |state| {
            format!(
                "TARGET {} @ {}/{}",
                if state.target_active { "BLACK" } else { "LIVE" },
                state.position.numerator(),
                FadeToBlackPosition::DENOMINATOR,
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
            fade_to_black_label(Some(state(true, 40_000))),
            "TARGET BLACK @ 40000/65535"
        );
        assert_eq!(
            fade_to_black_label(Some(state(false, 20_000))),
            "TARGET LIVE @ 20000/65535"
        );
        assert_eq!(fade_to_black_label(None), "UNAVAILABLE");
        assert_eq!(
            fade_to_black_short_label(Some(state(true, 65_535))),
            "BLACK 100%"
        );
        assert_eq!(fade_to_black_short_label(Some(state(false, 0))), "LIVE 0%");
        assert_eq!(fade_to_black_short_label(None), "UNAVAILABLE");
    }
}
