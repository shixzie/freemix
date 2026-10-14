//! The transition column between the Preview and Program monitors: automatic
//! transitions, shared duration, the manual T-bar, and Fade-to-Black.

use egui::{CornerRadius, DragValue, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, pos2};
use fm_protocol::{ManualTransitionKind, ManualTransitionPosition};
use fm_ui_model::ManualTransitionStatus;

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg, with_key};
use crate::theme::{
    ACCENT_FILL, AMBER, AMBER_FILL, MONITOR_BLACK, accent_color, border_color, border_strong_color,
    console_button, field_label, mono, muted_color, section_label, text_color, text_dim_color,
    widget_active_color, widget_color, widget_hover_color,
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
    let l = l10n::locale(ui.ctx());
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
        ui.label(section_label(l.text(Msg::TransitionSection)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let active = matches!(desired, ManualTransitionStatus::Active(_));
            ui.label(
                mono(if active {
                    l.format(
                        Msg::TBarPercent,
                        &[(
                            "percent",
                            Arg::Str(&l.percent(i64::from(
                                desired_manual_position(desired).basis_points() / 100,
                            ))),
                        )],
                    )
                } else {
                    l.text(Msg::TBar).to_owned()
                })
                .size(crate::theme::legible(10.0))
                .strong()
                .extra_letter_spacing(1.0)
                .color(if active { AMBER } else { muted_color() }),
            );
        });
    });
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(2.0 * BUTTON_SIZE.x + 6.0);
            draw_auto_transitions(ui, shell, state, intents);
        });
        ui.add_space(4.0);
        draw_t_bar(
            ui,
            desired,
            manual.active_controls,
            manual.alpha_fade_start_exposed,
            intents,
        );
    });
    ui.separator();
    let session_reason = unavailable_reason(state, Capability::Transition);
    let manual_reason = if state.connection_status.controls_enabled()
        && state.view.is_some()
        && state.can_transition
    {
        Msg::ReasonManualStartFirst.en()
    } else {
        session_reason
    };
    let names = state.view.as_ref().map_or(
        InputNames {
            locale: l,
            inputs: &[],
            names: &[],
        },
        |view| InputNames {
            locale: l,
            inputs: &view.inputs,
            names: &view.input_names,
        },
    );
    draw_manual_transition(
        ui,
        (desired, realized),
        (manual, &mut shell.layout.show_manual_details),
        (session_reason, manual_reason),
        names,
        intents,
    );
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
    let l = l10n::locale(ui.ctx());
    let reason = unavailable_reason(state, Capability::Transition);
    let duration_frames = shell.transition_duration_frames;
    let primary = |label: &str| {
        console_button(
            label,
            ACCENT_FILL,
            accent_color().gamma_multiply(0.6),
            BUTTON_SIZE,
        )
    };
    let secondary =
        |label: &str| console_button(label, widget_color(), border_strong_color(), BUTTON_SIZE);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                availability.basic(),
                // The take key is the brightest neutral key, not red: red
                // on the panel means on air.
                console_button(
                    l.text(Msg::CutKey),
                    widget_active_color(),
                    text_color().gamma_multiply(0.7),
                    BUTTON_SIZE,
                ),
            )
            .hint(&with_key(l, l.text(Msg::ShortcutCut), "C"), reason)
            .clicked()
        {
            intents.push(StudioIntent::Cut);
        }
        if ui
            .add_enabled(availability.basic(), primary(l.text(Msg::FadeKey)))
            .hint(&with_key(l, l.text(Msg::AutoFadeHover), "F"), reason)
            .clicked()
        {
            intents.push(StudioIntent::Fade { duration_frames });
        }
    });
    ui.horizontal(|ui| {
        if ui
            .add_enabled(availability.wipe, secondary(l.text(Msg::WipeKey)))
            .hint(l.text(Msg::AutoWipeHover), reason)
            .clicked()
        {
            intents.push(StudioIntent::Wipe { duration_frames });
        }
        if ui
            .add_enabled(availability.alpha_fade, secondary(l.text(Msg::AlphaKey)))
            .hint(l.text(Msg::AutoAlphaHover), reason)
            .clicked()
        {
            intents.push(StudioIntent::AlphaFade { duration_frames });
        }
    });
    ui.horizontal(|ui| {
        if ui
            .add_enabled(availability.slide(), secondary(l.text(Msg::SlideKey)))
            .hint(l.text(Msg::AutoSlideHover), reason)
            .clicked()
        {
            intents.push(StudioIntent::Slide { duration_frames });
        }
        if ui
            .add_enabled(availability.zoom(), secondary(l.text(Msg::ZoomKey)))
            .hint(l.text(Msg::AutoZoomHover), reason)
            .clicked()
        {
            intents.push(StudioIntent::Zoom { duration_frames });
        }
    });
    ui.horizontal(|ui| {
        ui.label(field_label(l.text(Msg::Duration)));
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
            .hint(l.text(Msg::DurationHover), reason);
        });
    });
    shell.set_transition_duration_frames(shell.transition_duration_frames);
}

/// The manual block: start keys always; the exact position, Commit and
/// Cancel keys, and status card while a manual transition is desired or
/// realized, or while the operator keeps the details open.
fn draw_manual_transition(
    ui: &mut Ui,
    (desired, realized): (ManualTransitionStatus, ManualTransitionStatus),
    (availability, details): (ManualTransitionAvailability, &mut bool),
    (start_reason, active_reason): (&str, &str),
    names: InputNames<'_>,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(field_label(l.text(Msg::ManualLabel)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            crate::theme::disclosure(
                ui,
                details,
                l.text(Msg::DetailsKey),
                (
                    l.text(Msg::DisclosureShowManual),
                    l.text(Msg::DisclosureHideManual),
                ),
            );
        });
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        draw_manual_start_buttons(
            ui,
            availability.start,
            availability.alpha_fade_start_exposed,
            start_reason,
            intents,
        );
    });
    let in_flight = matches!(desired, ManualTransitionStatus::Active(_))
        || matches!(realized, ManualTransitionStatus::Active(_));
    if !(*details || in_flight) {
        return;
    }
    ui.horizontal(|ui| {
        draw_manual_position(
            ui,
            desired,
            availability.active_controls,
            active_reason,
            intents,
        );
        draw_manual_terminal_buttons(ui, availability.active_controls, active_reason, intents);
    });
    draw_manual_status(
        ui,
        &manual_summary(desired, realized, names),
        desired,
        realized,
        availability.alpha_fade_start_exposed,
    );
}

/// Operator-facing summary of the manual transition, derived only from the
/// replicated desired and realized manual-transition state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManualSummary {
    /// The desired transition kind as catalog English (worded in the
    /// console language when drawn), or `None` while no manual transition
    /// is desired.
    pub(crate) kind: Option<&'static str>,
    /// Source and destination input names of the desired transition.
    pub(crate) route: Option<(String, String)>,
    /// Desired lever travel in whole percent (0 = start, 100 = end).
    pub(crate) percent: u16,
    /// Whether the last realized state matches the desired state.
    pub(crate) confirmed: bool,
}

/// Input names, in project order, used to label manual-transition routes.
#[derive(Clone, Copy)]
pub(crate) struct InputNames<'a> {
    pub(crate) locale: Locale,
    pub(crate) inputs: &'a [fm_types::InputId],
    pub(crate) names: &'a [String],
}

impl InputNames<'_> {
    fn name(self, input: fm_types::InputId) -> String {
        crate::monitors::input_name(self.locale, self.inputs, self.names, input)
    }
}

pub(crate) fn manual_summary(
    desired: ManualTransitionStatus,
    realized: ManualTransitionStatus,
    names: InputNames<'_>,
) -> ManualSummary {
    // The interval start is bookkeeping for reversal, not operator state.
    let confirmed = match (desired, realized) {
        (ManualTransitionStatus::Inactive, ManualTransitionStatus::Inactive) => true,
        (ManualTransitionStatus::Active(desired), ManualTransitionStatus::Active(realized)) => {
            desired.kind == realized.kind
                && desired.from == realized.from
                && desired.to == realized.to
                && desired.position == realized.position
        }
        _ => false,
    };
    match desired {
        ManualTransitionStatus::Inactive => ManualSummary {
            kind: None,
            route: None,
            percent: 0,
            confirmed,
        },
        ManualTransitionStatus::Active(state) => ManualSummary {
            kind: Some(manual_kind_label(state.kind)),
            route: Some((names.name(state.from), names.name(state.to))),
            percent: state.position.basis_points() / 100,
            confirmed,
        },
    }
}

const fn manual_kind_label(kind: ManualTransitionKind) -> &'static str {
    match kind {
        ManualTransitionKind::Fade => Msg::FadeKey.en(),
        ManualTransitionKind::Wipe => Msg::WipeKey.en(),
        ManualTransitionKind::AlphaFade => Msg::AlphaFadeKind.en(),
        ManualTransitionKind::Slide => Msg::SlideKey.en(),
    }
}

/// The manual start keys in console order, joined for hint text
/// (`FADE · WIPE · SLIDE · ALPHA`).
fn manual_start_keys(l: Locale, alpha_fade_exposed: bool, separator: &str) -> String {
    let mut keys = vec![
        l.text(Msg::FadeKey),
        l.text(Msg::WipeKey),
        l.text(Msg::SlideKey),
    ];
    if alpha_fade_exposed {
        keys.push(l.text(Msg::AlphaKey));
    }
    keys.join(separator)
}

/// A compact status card: kind, route, travel, and daemon confirmation.
fn draw_manual_status(
    ui: &mut Ui,
    summary: &ManualSummary,
    desired: ManualTransitionStatus,
    realized: ManualTransitionStatus,
    alpha_fade_exposed: bool,
) {
    let l = l10n::locale(ui.ctx());
    let active = summary.kind.is_some();
    let accent = if active { AMBER } else { border_strong_color() };
    let response = egui::Frame::new()
        .fill(if active { AMBER_FILL } else { MONITOR_BLACK })
        .stroke(Stroke::new(1.0, accent.gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(l.tr(summary.kind.unwrap_or(Msg::NoManualTransition.en())))
                        .size(crate::theme::legible(10.5))
                        .strong()
                        .extra_letter_spacing(0.8)
                        .color(if active { AMBER } else { muted_color() }),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if active {
                        ui.label(
                            mono(l.percent(i64::from(summary.percent)))
                                .size(crate::theme::legible(11.0))
                                .strong()
                                .color(text_color()),
                        );
                    }
                    if !summary.confirmed {
                        ui.label(
                            RichText::new(l.text(Msg::Syncing))
                                .size(crate::theme::legible(9.5))
                                .strong()
                                .color(AMBER),
                        );
                    }
                });
            });
            // Both states keep the same rows so the column never jumps.
            let (route, color) = summary.route.as_ref().map_or_else(
                || {
                    (
                        l.format(
                            Msg::ManualStartHint,
                            &[(
                                "keys",
                                Arg::Str(&manual_start_keys(l, alpha_fade_exposed, " · ")),
                            )],
                        ),
                        muted_color(),
                    )
                },
                |(from, to)| (format!("{from}  ▶  {to}"), text_color()),
            );
            ui.add(
                egui::Label::new(
                    // Fixed at 11 pt so the start hint fits the column in
                    // every theme.
                    RichText::new(route).size(11.0).color(color),
                )
                .truncate(),
            );
            {
                let (bar, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 3.0), Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(bar, CornerRadius::same(1), border_color());
                painter.rect_filled(
                    Rect::from_min_size(
                        bar.min,
                        Vec2::new(
                            bar.width() * f32::from(summary.percent) / 100.0,
                            bar.height(),
                        ),
                    ),
                    CornerRadius::same(1),
                    AMBER,
                );
            }
        })
        .response;
    let mut hover = l.format(
        Msg::ManualStatusHover,
        &[
            ("desired", Arg::Str(&manual_transition_label(l, desired))),
            ("realized", Arg::Str(&manual_transition_label(l, realized))),
        ],
    );
    if !summary.confirmed {
        hover.push('\n');
        hover.push_str(l.text(Msg::ManualUnconfirmed));
    }
    response.on_hover_text(hover);
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
    alpha_fade_exposed: bool,
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
    let response = response.on_hover_text(t_bar_hover(
        l10n::locale(ui.ctx()),
        enabled,
        alpha_fade_exposed,
    ));
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
        Stroke::new(crate::theme::line_width(), border_color()),
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
            Stroke::new(crate::theme::line_width(), border_strong_color()),
        );
        let x = rect.max.x - 2.0;
        painter.line_segment(
            [pos2(x - half + 4.0, y), pos2(x, y)],
            Stroke::new(crate::theme::line_width(), border_strong_color()),
        );
    }
    let handle = Rect::from_center_size(pos2(rect.center().x, handle_y), HANDLE);
    let (fill, stroke) = if !enabled {
        (widget_color(), border_color())
    } else if response.hovered() || response.dragged() {
        (widget_active_color(), AMBER)
    } else {
        (widget_hover_color(), border_strong_color())
    };
    painter.rect_filled(handle, CornerRadius::same(3), fill);
    painter.rect_stroke(
        handle,
        CornerRadius::same(3),
        Stroke::new(crate::theme::line_width(), stroke),
        StrokeKind::Inside,
    );
    for offset in [-3.0, 0.0, 3.0] {
        painter.line_segment(
            [
                pos2(handle.min.x + 8.0, handle_y + offset),
                pos2(handle.max.x - 8.0, handle_y + offset),
            ],
            Stroke::new(
                crate::theme::line_width(),
                if enabled {
                    text_dim_color()
                } else {
                    muted_color()
                },
            ),
        );
    }
}

/// The T-bar tooltip: how to drive the lever, or how to enable it.
fn t_bar_hover(l: Locale, enabled: bool, alpha_fade_exposed: bool) -> String {
    if enabled {
        l.format(
            Msg::TBarHoverActive,
            &[
                ("commit", Arg::Str(l.text(Msg::CommitKey))),
                ("cancel", Arg::Str(l.text(Msg::CancelKey))),
            ],
        )
    } else {
        l.format(
            Msg::TBarHoverIdle,
            &[(
                "keys",
                Arg::Str(&manual_start_keys(l, alpha_fade_exposed, ", ")),
            )],
        )
    }
}

fn draw_manual_start_buttons(
    ui: &mut Ui,
    enabled: bool,
    alpha_fade_exposed: bool,
    reason: &str,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let mut keys = vec![
        (
            Msg::FadeKey,
            Msg::TransitionFade,
            ManualTransitionKind::Fade,
        ),
        (
            Msg::WipeKey,
            Msg::TransitionWipe,
            ManualTransitionKind::Wipe,
        ),
        (
            Msg::SlideKey,
            Msg::TransitionSlide,
            ManualTransitionKind::Slide,
        ),
    ];
    if alpha_fade_exposed {
        keys.push((
            Msg::AlphaKey,
            Msg::TransitionAlphaFade,
            ManualTransitionKind::AlphaFade,
        ));
    }
    // Equal-width keys that always fill the column, whatever the language.
    #[allow(clippy::cast_precision_loss)] // three or four keys
    let count = keys.len() as f32;
    let spacing = ui.spacing().item_spacing.x;
    let size = Vec2::new(
        ((ui.available_width() - spacing * (count - 1.0)) / count).floor(),
        22.0,
    );
    ui.spacing_mut().button_padding.x = 2.0;
    for (label, name, kind) in keys {
        let tooltip = l.format(Msg::ManualStartHover, &[("kind", Arg::Str(l.text(name)))]);
        let response = ui
            .add_enabled(enabled, start_button(l.text(label), size))
            .hint(&tooltip, reason);
        if response.clicked() {
            intents.push(StudioIntent::StartManualTransition { kind });
        }
    }
}

fn start_button(label: &str, size: Vec2) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(label.to_owned())
            .size(crate::theme::legible(10.0))
            .color(text_color()),
    )
    .min_size(size)
    .truncate()
}

fn draw_manual_position(
    ui: &mut Ui,
    desired: ManualTransitionStatus,
    enabled: bool,
    reason: &str,
    intents: &mut Vec<StudioIntent>,
) {
    {
        let l = l10n::locale(ui.ctx());
        let position_label = ui.label(field_label(l.text(Msg::PositionShort)));
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
            .hint(l.text(Msg::PositionHover), reason);
        if response.changed() {
            intents.push(StudioIntent::SetManualTransitionPosition {
                position: ManualTransitionPosition::new(position)
                    .expect("DragValue range bounds manual positions"),
            });
        }
    }
}

fn draw_manual_terminal_buttons(
    ui: &mut Ui,
    enabled: bool,
    reason: &str,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    // The two keys share the rest of the row equally.
    let spacing = ui.spacing().item_spacing.x;
    let width = ((ui.available_width() - spacing) / 2.0).floor().max(40.0);
    ui.spacing_mut().button_padding.x = 3.0;
    for (label, tooltip, intent) in [
        (
            l.text(Msg::CommitKey),
            l.text(Msg::CommitHover).to_owned(),
            StudioIntent::CommitManualTransition,
        ),
        (
            l.text(Msg::CancelKey),
            with_key(l, l.text(Msg::ShortcutCancelManual), "Esc"),
            StudioIntent::CancelManualTransition,
        ),
    ] {
        let response = ui
            .add_enabled(
                enabled,
                egui::Button::new(
                    RichText::new(label)
                        .size(crate::theme::legible(10.5))
                        .strong(),
                )
                .min_size(Vec2::new(width, 22.0))
                .truncate(),
            )
            .hint(&tooltip, reason);
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

pub(crate) fn manual_transition_label(l: Locale, status: ManualTransitionStatus) -> String {
    match status {
        ManualTransitionStatus::Inactive => l.text(Msg::ManualInactive).to_owned(),
        ManualTransitionStatus::Active(state) => l.format(
            Msg::ManualStateActive,
            &[
                ("kind", Arg::Str(l.tr(manual_kind_label(state.kind)))),
                ("from", Arg::Str(&state.from.to_string())),
                ("to", Arg::Str(&state.to.to_string())),
                (
                    "position",
                    Arg::Plain(u64::from(state.position.basis_points())),
                ),
            ],
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

    fn input(value: u128) -> fm_types::InputId {
        fm_types::InputId::new(core::num::NonZeroU128::new(value).unwrap())
    }

    fn active(position: u16) -> ManualTransitionStatus {
        active_from(position, 0)
    }

    fn active_from(position: u16, interval_start: u16) -> ManualTransitionStatus {
        ManualTransitionStatus::Active(fm_ui_model::ActiveManualTransition {
            kind: ManualTransitionKind::Slide,
            from: input(1),
            to: input(9),
            interval_start: ManualTransitionPosition::new(interval_start).unwrap(),
            position: ManualTransitionPosition::new(position).unwrap(),
        })
    }

    #[test]
    fn manual_summary_names_the_route_travel_and_confirmation() {
        let inputs = [input(1), input(2)];
        let names = ["Camera 1".to_owned(), "Camera 2".to_owned()];
        let names = InputNames {
            locale: Locale::ENGLISH,
            inputs: &inputs,
            names: &names,
        };
        let idle = ManualTransitionStatus::Inactive;
        assert_eq!(
            manual_summary(idle, idle, names),
            ManualSummary {
                kind: None,
                route: None,
                percent: 0,
                confirmed: true,
            }
        );
        let summary = manual_summary(active(4_750), active(4_750), names);
        assert_eq!(summary.kind, Some("SLIDE"));
        assert_eq!(
            summary.route,
            Some(("Camera 1".to_owned(), "Input 9".to_owned()))
        );
        assert_eq!(summary.percent, 47);
        assert!(summary.confirmed);
        // Desired ahead of realized: the card reports it as unconfirmed.
        assert!(!manual_summary(active(10_000), active(4_750), names).confirmed);
        assert_eq!(manual_summary(active(10_000), idle, names).percent, 100);
        assert!(!manual_summary(idle, active(4_750), names).confirmed);
        // Only operator-visible fields decide confirmation.
        assert!(manual_summary(active(4_750), active_from(4_750, 8_000), names).confirmed);
    }
}
