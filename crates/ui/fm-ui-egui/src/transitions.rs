//! The transition column between the Preview and Program monitors: automatic
//! transitions, shared duration, the manual T-bar, and Fade-to-Black.

use egui::{CornerRadius, DragValue, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, pos2};
use fm_protocol::{ManualTransitionKind, ManualTransitionPosition};
use fm_ui_model::ManualTransitionStatus;

use crate::theme::{
    ACCENT, ACCENT_FILL, AMBER, BORDER, BORDER_STRONG, MONITOR_BLACK, MUTED, PROGRAM, PROGRAM_FILL,
    TEXT, TEXT_DIM, WIDGET, WIDGET_ACTIVE, WIDGET_HOVER, console_button, mono, section_label,
};
use crate::{
    ManualTransitionAvailability, StudioIntent, StudioShell, StudioUiState, TransitionGate,
    fade_to_black, transition_availability,
};

/// Fixed width of the transition column.
pub(crate) const TRANSITION_COLUMN_WIDTH: f32 = 252.0;
const BUTTON_SIZE: Vec2 = Vec2::new(94.0, 36.0);
const T_BAR_HEIGHT: f32 = 148.0;

pub(crate) fn draw_transition_column(
    ui: &mut Ui,
    shell: &mut StudioShell,
    state: &StudioUiState,
    manual: ManualTransitionAvailability,
    intents: &mut Vec<StudioIntent>,
) {
    ui.set_width(TRANSITION_COLUMN_WIDTH);
    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
    let desired = state
        .view
        .as_ref()
        .map_or(ManualTransitionStatus::Inactive, |view| {
            view.switcher.desired_manual_transition
        });
    let realized = state
        .view
        .as_ref()
        .map_or(ManualTransitionStatus::Inactive, |view| {
            view.switcher.realized_manual_transition
        });
    ui.horizontal(|ui| {
        ui.label(section_label("Transition"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let active = matches!(desired, ManualTransitionStatus::Active(_));
            ui.label(
                RichText::new(if active { "T-BAR ACTIVE" } else { "T-BAR" })
                    .size(10.0)
                    .strong()
                    .extra_letter_spacing(1.0)
                    .color(if active { AMBER } else { MUTED }),
            );
        });
    });
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(2.0 * BUTTON_SIZE.x + 6.0);
            draw_auto_transitions(ui, shell, state, intents);
        });
        ui.add_space(4.0);
        draw_t_bar(ui, desired, manual.active_controls, intents);
    });
    ui.separator();
    draw_manual_transition(ui, desired, realized, manual, intents);
    ui.separator();
    fade_to_black::draw_fade_to_black(ui, &mut shell.fade_to_black_duration_frames, state, intents);
    shell.set_fade_to_black_duration_frames(shell.fade_to_black_duration_frames);
}

fn draw_auto_transitions(
    ui: &mut Ui,
    shell: &mut StudioShell,
    state: &StudioUiState,
    intents: &mut Vec<StudioIntent>,
) {
    let availability = transition_availability(TransitionGate::from_state(state));
    let duration_frames = shell.transition_duration_frames;
    let primary =
        |label: &str| console_button(label, ACCENT_FILL, ACCENT.gamma_multiply(0.6), BUTTON_SIZE);
    let secondary = |label: &str| console_button(label, WIDGET, BORDER_STRONG, BUTTON_SIZE);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                availability.basic(),
                console_button(
                    "CUT",
                    PROGRAM_FILL,
                    PROGRAM.gamma_multiply(0.7),
                    BUTTON_SIZE,
                ),
            )
            .on_hover_text("Cut Preview to Program  [C]")
            .clicked()
        {
            intents.push(StudioIntent::Cut);
        }
        if ui
            .add_enabled(availability.basic(), primary("FADE"))
            .on_hover_text("Fade Preview to Program  [F]")
            .clicked()
        {
            intents.push(StudioIntent::Fade { duration_frames });
        }
    });
    ui.horizontal(|ui| {
        if ui
            .add_enabled(availability.wipe, secondary("WIPE"))
            .on_hover_text("Wipe Preview to Program")
            .clicked()
        {
            intents.push(StudioIntent::Wipe { duration_frames });
        }
        if ui
            .add_enabled(availability.alpha_fade, secondary("ALPHA"))
            .on_hover_text("AlphaFade Preview to Program")
            .clicked()
        {
            intents.push(StudioIntent::AlphaFade { duration_frames });
        }
    });
    ui.horizontal(|ui| {
        if ui
            .add_enabled(availability.slide(), secondary("SLIDE"))
            .on_hover_text("Slide Preview to Program")
            .clicked()
        {
            intents.push(StudioIntent::Slide { duration_frames });
        }
        if ui
            .add_enabled(availability.zoom(), secondary("ZOOM"))
            .on_hover_text("Zoom Preview to Program")
            .clicked()
        {
            intents.push(StudioIntent::Zoom { duration_frames });
        }
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("DURATION").size(10.5).color(TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_enabled(
                availability.basic(),
                DragValue::new(&mut shell.transition_duration_frames)
                    .range(
                        StudioShell::MIN_TRANSITION_DURATION_FRAMES
                            ..=StudioShell::MAX_TRANSITION_DURATION_FRAMES,
                    )
                    .suffix(" fr")
                    .speed(1.0),
            )
            .on_hover_text("Shared automatic transition duration in frames");
        });
    });
    shell.set_transition_duration_frames(shell.transition_duration_frames);
}

fn draw_manual_transition(
    ui: &mut Ui,
    desired: ManualTransitionStatus,
    realized: ManualTransitionStatus,
    availability: ManualTransitionAvailability,
    intents: &mut Vec<StudioIntent>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(RichText::new("MANUAL").size(10.0).color(MUTED));
        draw_manual_start_buttons(
            ui,
            availability.start,
            availability.alpha_fade_start_exposed,
            intents,
        );
    });
    ui.horizontal(|ui| {
        draw_manual_position(ui, desired, availability.active_controls, intents);
        draw_manual_terminal_buttons(ui, availability.active_controls, intents);
    });
    ui.add(
        egui::Label::new(
            mono(format!(
                "D {} · R {}",
                manual_transition_label(desired),
                manual_transition_label(realized)
            ))
            .size(9.5)
            .color(TEXT_DIM),
        )
        .wrap(),
    )
    .on_hover_text("Desired (D) and last confirmed realized (R) manual transition");
}

/// Converts a T-bar travel fraction (0 = start, 1 = end) to a position.
pub(crate) fn t_bar_position(fraction: f32) -> ManualTransitionPosition {
    let max = f32::from(ManualTransitionPosition::MAX);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=MAX
    let basis_points = (fraction.clamp(0.0, 1.0) * max).round() as u16;
    ManualTransitionPosition::new(basis_points).expect("clamped fraction stays within range")
}

/// The vertical T-bar lever bound to the replicated desired manual position.
///
/// The lever always draws the replicated desired position; dragging emits
/// position intents and the lever follows once the daemon confirms them.
fn draw_t_bar(
    ui: &mut Ui,
    desired: ManualTransitionStatus,
    enabled: bool,
    intents: &mut Vec<StudioIntent>,
) {
    const WIDTH: f32 = 40.0;
    const HANDLE: Vec2 = Vec2::new(36.0, 16.0);
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(WIDTH, T_BAR_HEIGHT), sense);
    let travel_top = rect.min.y + HANDLE.y / 2.0;
    let travel_bottom = rect.max.y - HANDLE.y / 2.0;
    let moved = response.drag_started()
        || response.clicked()
        || (response.dragged() && response.drag_delta() != Vec2::ZERO);
    if enabled
        && moved
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let fraction = (travel_bottom - pointer.y) / (travel_bottom - travel_top);
        let position = t_bar_position(fraction);
        if position != desired_manual_position(desired) {
            intents.push(StudioIntent::SetManualTransitionPosition { position });
        }
    }
    let response = response.on_hover_text(
        "Manual T-bar: start a manual transition, then drag the lever (bottom = start, top = end)",
    );
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect.expand(2.0));
    let fraction = f32::from(desired_manual_position(desired).basis_points())
        / f32::from(ManualTransitionPosition::MAX);
    let handle_y = travel_bottom - fraction * (travel_bottom - travel_top);
    let groove = Rect::from_center_size(
        pos2(rect.center().x, rect.center().y),
        Vec2::new(8.0, T_BAR_HEIGHT - 4.0),
    );
    painter.rect_filled(groove, CornerRadius::same(4), MONITOR_BLACK);
    painter.rect_stroke(
        groove,
        CornerRadius::same(4),
        Stroke::new(1.0, BORDER),
        StrokeKind::Outside,
    );
    if fraction > 0.0 {
        let fill = Rect::from_min_max(
            pos2(groove.min.x + 1.0, handle_y),
            pos2(groove.max.x - 1.0, groove.max.y - 1.0),
        );
        painter.rect_filled(fill, CornerRadius::same(3), AMBER.gamma_multiply(0.75));
    }
    for tick in 0..=10 {
        #[allow(clippy::cast_precision_loss)]
        let y = travel_bottom - tick as f32 / 10.0 * (travel_bottom - travel_top);
        let half = if tick % 5 == 0 { 9.0 } else { 6.0 };
        let x = rect.min.x + 2.0;
        painter.line_segment(
            [pos2(x, y), pos2(x + half - 4.0, y)],
            Stroke::new(1.0, BORDER_STRONG),
        );
        let x = rect.max.x - 2.0;
        painter.line_segment(
            [pos2(x - half + 4.0, y), pos2(x, y)],
            Stroke::new(1.0, BORDER_STRONG),
        );
    }
    let handle = Rect::from_center_size(pos2(rect.center().x, handle_y), HANDLE);
    let (fill, stroke) = if !enabled {
        (WIDGET, BORDER)
    } else if response.hovered() || response.dragged() {
        (WIDGET_ACTIVE, AMBER)
    } else {
        (WIDGET_HOVER, BORDER_STRONG)
    };
    painter.rect_filled(handle, CornerRadius::same(3), fill);
    painter.rect_stroke(
        handle,
        CornerRadius::same(3),
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    );
    for offset in [-3.0, 0.0, 3.0] {
        painter.line_segment(
            [
                pos2(handle.min.x + 8.0, handle_y + offset),
                pos2(handle.max.x - 8.0, handle_y + offset),
            ],
            Stroke::new(1.0, if enabled { TEXT_DIM } else { MUTED }),
        );
    }
}

fn draw_manual_start_buttons(
    ui: &mut Ui,
    enabled: bool,
    alpha_fade_exposed: bool,
    intents: &mut Vec<StudioIntent>,
) {
    let size = Vec2::new(44.0, 22.0);
    for (label, kind, tooltip) in [
        (
            "FADE",
            ManualTransitionKind::Fade,
            "Start a reversible manual Fade transition",
        ),
        (
            "WIPE",
            ManualTransitionKind::Wipe,
            "Start a reversible manual Wipe transition",
        ),
        (
            "SLIDE",
            ManualTransitionKind::Slide,
            "Start a reversible manual Slide transition",
        ),
    ] {
        let response = ui
            .add_enabled(enabled, start_button(label, size))
            .on_hover_text(tooltip);
        if response.clicked() {
            intents.push(StudioIntent::StartManualTransition { kind });
        }
    }
    if alpha_fade_exposed {
        let response = ui
            .add_enabled(enabled, start_button("ALPHA", size))
            .on_hover_text("Start a reversible manual AlphaFade transition");
        if response.clicked() {
            intents.push(StudioIntent::StartManualTransition {
                kind: ManualTransitionKind::AlphaFade,
            });
        }
    }
}

fn start_button(label: &str, size: Vec2) -> egui::Button<'static> {
    egui::Button::new(RichText::new(label.to_owned()).size(10.5).color(TEXT)).min_size(size)
}

fn draw_manual_position(
    ui: &mut Ui,
    desired: ManualTransitionStatus,
    enabled: bool,
    intents: &mut Vec<StudioIntent>,
) {
    {
        let position_label = ui.label(RichText::new("POS").size(10.0).color(MUTED));
        let mut position = desired_manual_position(desired).basis_points();
        let response = ui
            .add_enabled(
                enabled,
                DragValue::new(&mut position)
                    .range(
                        ManualTransitionPosition::START.basis_points()
                            ..=ManualTransitionPosition::END.basis_points(),
                    )
                    .suffix(" bp"),
            )
            .labelled_by(position_label.id)
            .on_hover_text(
                "Exact T-bar position from 0 through 10000 basis points; lower values reverse",
            );
        if response.changed() {
            intents.push(StudioIntent::SetManualTransitionPosition {
                position: ManualTransitionPosition::new(position)
                    .expect("DragValue range bounds manual positions"),
            });
        }
    }
}

fn draw_manual_terminal_buttons(ui: &mut Ui, enabled: bool, intents: &mut Vec<StudioIntent>) {
    for (label, tooltip, intent) in [
        (
            "COMMIT",
            "Commit the active manual transition",
            StudioIntent::CommitManualTransition,
        ),
        (
            "CANCEL",
            "Cancel the active manual transition  [Esc]",
            StudioIntent::CancelManualTransition,
        ),
    ] {
        let response = ui
            .add_enabled(
                enabled,
                egui::Button::new(RichText::new(label).size(10.5).strong())
                    .min_size(Vec2::new(0.0, 22.0)),
            )
            .on_hover_text(tooltip);
        if response.clicked() {
            intents.push(intent);
        }
    }
}

pub(crate) const fn desired_manual_position(
    status: ManualTransitionStatus,
) -> ManualTransitionPosition {
    match status {
        ManualTransitionStatus::Inactive => ManualTransitionPosition::START,
        ManualTransitionStatus::Active(state) => state.position,
    }
}

pub(crate) fn manual_transition_label(status: ManualTransitionStatus) -> String {
    match status {
        ManualTransitionStatus::Inactive => "INACTIVE".to_owned(),
        ManualTransitionStatus::Active(state) => format!(
            "{} {}->{} @ {} BP",
            match state.kind {
                ManualTransitionKind::Fade => "FADE",
                ManualTransitionKind::Wipe => "WIPE",
                ManualTransitionKind::AlphaFade => "ALPHA FADE",
                ManualTransitionKind::Slide => "SLIDE",
            },
            state.from,
            state.to,
            state.position.basis_points(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t_bar_fraction_maps_to_clamped_basis_points() {
        assert_eq!(t_bar_position(0.0), ManualTransitionPosition::START);
        assert_eq!(t_bar_position(-1.0), ManualTransitionPosition::START);
        assert_eq!(t_bar_position(1.0), ManualTransitionPosition::END);
        assert_eq!(t_bar_position(7.0), ManualTransitionPosition::END);
        assert_eq!(t_bar_position(0.25).basis_points(), 2_500);
    }
}
