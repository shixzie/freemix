//! The transition column between the Preview and Program monitors: automatic
//! transitions, shared duration, the manual T-bar, and Fade-to-Black.

use egui::{CornerRadius, DragValue, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, pos2};
use fm_protocol::{
    AutoTransitionKind, ManualTransitionKind, ManualTransitionPosition, OverlayTransitionKind,
    TransitionFavoriteEffect, TransitionFavoriteStatus, WireStingerSlotId,
};
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

/// Narrowest transition column: keys still hold their labels.
pub(crate) const TRANSITION_COLUMN_MIN_WIDTH: f32 = 200.0;
/// Widest transition column; a wider tile centres it.
pub(crate) const TRANSITION_COLUMN_MAX_WIDTH: f32 = 360.0;
/// Room kept right of the column for a short tile's floating scroll bar.
pub(crate) const SCROLL_GUTTER: f32 = 12.0;
/// Height of a transition key; keys widen with the column, not taller.
const BUTTON_HEIGHT: f32 = 36.0;
/// Width beside the two key columns: the T-bar, its gap, and spacing.
const T_BAR_COLUMN: f32 = 56.0;
/// Height of a favourite key.
const FAVORITE_HEIGHT: f32 = 28.0;

/// The transition column's width in `available` points: the tile's width
/// within the column's range.
pub(crate) fn column_width(available: f32) -> f32 {
    available
        .clamp(TRANSITION_COLUMN_MIN_WIDTH, TRANSITION_COLUMN_MAX_WIDTH)
        .floor()
}

/// Key sizes of a transition column `width` wide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ColumnMetrics {
    pub(crate) width: f32,
    /// One automatic transition key: two columns of them beside the T-bar.
    pub(crate) button: Vec2,
    /// One favourite key: the four keys and their three 4-point gaps share
    /// the column width.
    pub(crate) favorite: Vec2,
}

impl ColumnMetrics {
    pub(crate) fn new(width: f32) -> Self {
        let width = column_width(width);
        Self {
            width,
            button: Vec2::new(((width - T_BAR_COLUMN - 6.0) / 2.0).floor(), BUTTON_HEIGHT),
            favorite: Vec2::new(((width - 3.0 * 4.0) / 4.0).floor(), FAVORITE_HEIGHT),
        }
    }

    /// The width of both key columns with the gap between them.
    fn keys_width(self) -> f32 {
        2.0 * self.button.x + 6.0
    }
}
const _: () = assert!(fm_protocol::TRANSITION_FAVORITE_COUNT == 4);
/// Label size range of the favourite keys (points).
const FAVORITE_LABEL_MIN: f32 = 9.0;
/// Smallest label size of the manual start keys.
const MANUAL_LABEL_MIN: f32 = 8.0;
const FAVORITE_LABEL_MAX: f32 = 12.0;
/// The automatic mixes behind the MORE key: every kind without a key of
/// its own in the column.
const MORE_KINDS: [AutoTransitionKind; 8] = [
    AutoTransitionKind::VerticalWipe,
    AutoTransitionKind::VerticalSlide,
    AutoTransitionKind::Fly,
    AutoTransitionKind::FlyRotate,
    AutoTransitionKind::Cube,
    AutoTransitionKind::CubeZoom,
    AutoTransitionKind::CrossZoom,
    AutoTransitionKind::Merge,
];

/// The display names of one transition: `short` on console keys, where
/// width is scarce, and `name` (the full, localized name) in menus and
/// tooltips. Every surface takes its words from this one table, so a kind
/// reads the same in the column, MORE, the favourites' Assign menu, the
/// overlay picker, the Output menu, and the playlist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TransitionName {
    pub(crate) short: Msg,
    pub(crate) name: Msg,
}

const fn names(short: Msg, name: Msg) -> TransitionName {
    TransitionName { short, name }
}

/// The names of Cut.
pub(crate) const CUT_NAME: TransitionName = names(Msg::CutKey, Msg::TransitionCut);

/// The names of an automatic mix.
pub(crate) const fn mix_name(kind: AutoTransitionKind) -> TransitionName {
    match kind {
        AutoTransitionKind::Fade => names(Msg::FadeKey, Msg::TransitionFade),
        AutoTransitionKind::AlphaFade => names(Msg::AlphaKey, Msg::TransitionAlphaFade),
        AutoTransitionKind::Wipe => names(Msg::WipeKey, Msg::TransitionWipe),
        AutoTransitionKind::VerticalWipe => {
            names(Msg::VerticalWipeKey, Msg::TransitionVerticalWipe)
        }
        AutoTransitionKind::Slide => names(Msg::SlideKey, Msg::TransitionSlide),
        AutoTransitionKind::VerticalSlide => {
            names(Msg::VerticalSlideKey, Msg::TransitionVerticalSlide)
        }
        AutoTransitionKind::Zoom => names(Msg::ZoomKey, Msg::TransitionZoom),
        AutoTransitionKind::Fly => names(Msg::FlyKey, Msg::TransitionFly),
        AutoTransitionKind::FlyRotate => names(Msg::FlyRotateKey, Msg::TransitionFlyTurn),
        AutoTransitionKind::Cube => names(Msg::CubeKey, Msg::TransitionCube),
        AutoTransitionKind::CubeZoom => names(Msg::CubeZoomKey, Msg::TransitionCubeZoom),
        AutoTransitionKind::CrossZoom => names(Msg::CrossZoomKey, Msg::TransitionCrossZoom),
        AutoTransitionKind::Merge => names(Msg::MergeKey, Msg::TransitionMerge),
    }
}

/// The names of an overlay channel transition: Cut, or the mix of the
/// same motion.
pub(crate) const fn overlay_name(kind: OverlayTransitionKind) -> TransitionName {
    match kind {
        OverlayTransitionKind::Cut => CUT_NAME,
        OverlayTransitionKind::Fade => mix_name(AutoTransitionKind::Fade),
        OverlayTransitionKind::Wipe => mix_name(AutoTransitionKind::Wipe),
        OverlayTransitionKind::VerticalWipe => mix_name(AutoTransitionKind::VerticalWipe),
        OverlayTransitionKind::Slide => mix_name(AutoTransitionKind::Slide),
        OverlayTransitionKind::VerticalSlide => mix_name(AutoTransitionKind::VerticalSlide),
        OverlayTransitionKind::Zoom => mix_name(AutoTransitionKind::Zoom),
        OverlayTransitionKind::Fly => mix_name(AutoTransitionKind::Fly),
        OverlayTransitionKind::FlyRotate => mix_name(AutoTransitionKind::FlyRotate),
        OverlayTransitionKind::Cube => mix_name(AutoTransitionKind::Cube),
        OverlayTransitionKind::CubeZoom => mix_name(AutoTransitionKind::CubeZoom),
        OverlayTransitionKind::CrossZoom => mix_name(AutoTransitionKind::CrossZoom),
        OverlayTransitionKind::Merge => mix_name(AutoTransitionKind::Merge),
    }
}

/// The effect a manual (T-bar) transition kind draws: its mix, or its
/// Stinger slot.
pub(crate) const fn manual_effect(kind: ManualTransitionKind) -> TransitionFavoriteEffect {
    match (kind, kind.mix()) {
        (_, Some(mix)) => TransitionFavoriteEffect::Mix(mix),
        (ManualTransitionKind::Stinger { slot, .. }, None) => {
            TransitionFavoriteEffect::Stinger(slot)
        }
        (_, None) => TransitionFavoriteEffect::Cut,
    }
}

/// The manual (T-bar) form of a transition effect: a mix, or a Stinger
/// whose clip spans `duration_frames`; Cut has none.
#[must_use]
pub const fn manual_kind_for(
    effect: TransitionFavoriteEffect,
    duration_frames: u32,
) -> Option<ManualTransitionKind> {
    match effect {
        TransitionFavoriteEffect::Cut => None,
        TransitionFavoriteEffect::Mix(kind) => Some(ManualTransitionKind::from_mix(kind)),
        TransitionFavoriteEffect::Stinger(slot) => Some(ManualTransitionKind::Stinger {
            slot,
            duration_frames,
        }),
    }
}

/// The step `PgUp` and `PgDn` move an active T-bar by, in basis points.
pub const T_BAR_KEY_STEP_BASIS_POINTS: i32 = 1_000;

/// One T-bar gesture. The lever, the console keys, and an external
/// controller all reduce to these, so each sends the same commands for the
/// same move (SW-001, SW-004).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TBarGesture {
    /// Start a manual transition of this kind.
    Start(ManualTransitionKind),
    /// Move the lever to `position`, first starting `kind` when no manual
    /// transition is active.
    MoveTo {
        kind: ManualTransitionKind,
        position: ManualTransitionPosition,
    },
    /// Move the active lever by this many basis points; negative reverses.
    Step(i32),
    /// Commit the active manual transition.
    Commit,
    /// Cancel the active manual transition.
    Cancel,
}

/// The intents one T-bar `gesture` sends against the replicated `desired`
/// manual transition and the session's `availability`.
#[must_use]
pub fn t_bar_intents(
    desired: ManualTransitionStatus,
    availability: ManualTransitionAvailability,
    gesture: TBarGesture,
) -> Vec<StudioIntent> {
    let current = desired_manual_position(desired);
    let active = availability.active_controls;
    match gesture {
        TBarGesture::Start(kind) if availability.start => {
            vec![StudioIntent::StartManualTransition { kind }]
        }
        TBarGesture::MoveTo { position, .. } if active => {
            if position == current {
                Vec::new()
            } else {
                vec![StudioIntent::SetManualTransitionPosition { position }]
            }
        }
        TBarGesture::MoveTo { kind, position } if availability.start => {
            let mut intents = vec![StudioIntent::StartManualTransition { kind }];
            if position != ManualTransitionPosition::START {
                intents.push(StudioIntent::SetManualTransitionPosition { position });
            }
            intents
        }
        TBarGesture::Step(delta) if active => {
            let target = (i32::from(current.basis_points()) + delta)
                .clamp(0, i32::from(ManualTransitionPosition::MAX));
            match u16::try_from(target)
                .ok()
                .and_then(ManualTransitionPosition::new)
            {
                Some(position) if position != current => {
                    vec![StudioIntent::SetManualTransitionPosition { position }]
                }
                _ => Vec::new(),
            }
        }
        TBarGesture::Commit if active => vec![StudioIntent::CommitManualTransition],
        TBarGesture::Cancel if active => vec![StudioIntent::CancelManualTransition],
        _ => Vec::new(),
    }
}

/// The T-bar gesture a bare console key performs: `T` starts the selected
/// kind, `PgUp` and `PgDn` step the lever, `End` commits, and `Esc` cancels.
#[must_use]
pub const fn t_bar_key_gesture(key: egui::Key, kind: ManualTransitionKind) -> Option<TBarGesture> {
    Some(match key {
        egui::Key::T => TBarGesture::Start(kind),
        egui::Key::PageUp => TBarGesture::Step(T_BAR_KEY_STEP_BASIS_POINTS),
        egui::Key::PageDown => TBarGesture::Step(-T_BAR_KEY_STEP_BASIS_POINTS),
        egui::Key::End => TBarGesture::Commit,
        egui::Key::Escape => TBarGesture::Cancel,
        _ => return None,
    })
}

/// The overlay transition after `kind` in [`OverlayTransitionKind::ALL`],
/// wrapping from the last motion back to Cut.
#[must_use]
pub fn next_overlay_transition(kind: OverlayTransitionKind) -> OverlayTransitionKind {
    let all = OverlayTransitionKind::ALL;
    let index = all
        .iter()
        .position(|candidate| *candidate == kind)
        .unwrap_or(0);
    all[(index + 1) % all.len()]
}

/// The key label of a favourite's effect (`CUBE ZOOM`, `STING 1`).
pub(crate) fn effect_short(l: Locale, effect: TransitionFavoriteEffect) -> String {
    effect_text(l, effect, |names| names.short, Msg::StingerFavoriteKey)
}

/// The full name of a favourite's effect for menus and tooltips (`Cube
/// zoom`, `Stinger S1`).
pub(crate) fn effect_name(l: Locale, effect: TransitionFavoriteEffect) -> String {
    effect_text(l, effect, |names| names.name, Msg::TransitionStinger)
}

fn effect_text(
    l: Locale,
    effect: TransitionFavoriteEffect,
    pick: fn(TransitionName) -> Msg,
    stinger: Msg,
) -> String {
    match effect {
        TransitionFavoriteEffect::Cut => l.text(pick(CUT_NAME)).to_owned(),
        TransitionFavoriteEffect::Mix(kind) => l.text(pick(mix_name(kind))).to_owned(),
        TransitionFavoriteEffect::Stinger(slot) => {
            l.format(stinger, &[("slot", Arg::from(u32::from(slot.number())))])
        }
    }
}

/// The intent a favourite key performs: Cut, an automatic mix at the
/// favourite's duration, or its Stinger slot.
pub(crate) const fn favorite_intent(favorite: TransitionFavoriteStatus) -> StudioIntent {
    let duration_frames = favorite.duration_frames;
    match favorite.effect {
        TransitionFavoriteEffect::Cut => StudioIntent::Cut,
        TransitionFavoriteEffect::Mix(kind) => StudioIntent::Transition {
            kind,
            duration_frames,
        },
        TransitionFavoriteEffect::Stinger(slot) => StudioIntent::Stinger {
            slot,
            duration_frames,
        },
    }
}

/// The four favourite keys: a click takes the favourite, F5 through F8
/// take it from the keyboard, and right-click assigns it.
#[allow(clippy::too_many_lines)] // one panel section, top to bottom
fn draw_favorites(
    ui: &mut Ui,
    metrics: ColumnMetrics,
    shell: &mut StudioShell,
    (state, start): (&StudioUiState, (bool, &str)),
    intents: &mut Vec<StudioIntent>,
) {
    let t_bar_kind = &mut shell.t_bar_kind;
    let l = l10n::locale(ui.ctx());
    let availability = transition_availability(TransitionGate::from_state(state));
    let reason = unavailable_reason(state, Capability::Transition);
    let assign_enabled =
        state.connection_status.controls_enabled() && state.view.is_some() && state.can_transition;
    let favorites = state
        .view
        .as_ref()
        .map_or_else(TransitionFavoriteStatus::defaults, |view| {
            view.transition_favorites
        });
    let stingers: Vec<WireStingerSlotId> = state
        .view
        .as_ref()
        .map(|view| {
            view.stingers
                .iter()
                .filter_map(|stinger| WireStingerSlotId::new(stinger.slot))
                .collect()
        })
        .unwrap_or_default();
    let duration_frames = shell.transition_duration_frames;
    ui.label(field_label(l.text(Msg::FavoritesLabel)));
    let labels = favorites.map(|favorite| effect_short(l, favorite.effect));
    // One label size for the four keys: the largest at which every label
    // fits its fixed-width key. A label still too long at the smallest
    // size ends in an ellipsis; the tooltip names the effect in full.
    let label_size = labels
        .iter()
        .map(|label| {
            fitted_label_size(
                ui,
                label,
                (FAVORITE_LABEL_MIN, FAVORITE_LABEL_MAX),
                metrics.favorite.x - 6.0,
            )
        })
        .fold(FAVORITE_LABEL_MAX, f32::min);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (index, (favorite, label)) in favorites.into_iter().zip(labels).enumerate() {
            let slot = u8::try_from(index + 1).expect("four favourites");
            let enabled = match favorite.effect {
                TransitionFavoriteEffect::Stinger(_) => availability.stinger(),
                _ => availability.basic(),
            };
            let action = match favorite.effect {
                TransitionFavoriteEffect::Cut => l.format(
                    Msg::FavoriteCutHover,
                    &[("slot", Arg::from(u32::from(slot)))],
                ),
                effect => l.format(
                    Msg::FavoriteHover,
                    &[
                        ("slot", Arg::from(u32::from(slot))),
                        ("effect", Arg::Str(&effect_name(l, effect))),
                        (
                            "frames",
                            Arg::Str(&l10n::frames(l, favorite.duration_frames)),
                        ),
                    ],
                ),
            };
            let response = ui
                .add_enabled_ui(enabled, |ui| {
                    ui.spacing_mut().button_padding.x = 3.0;
                    ui.add_sized(
                        metrics.favorite,
                        egui::Button::new(
                            RichText::new(label)
                                .strong()
                                .size(label_size)
                                .color(text_color()),
                        )
                        .fill(widget_color())
                        .stroke(Stroke::new(
                            crate::theme::line_width(),
                            border_strong_color(),
                        ))
                        .corner_radius(CornerRadius::same(crate::theme::RADIUS))
                        .min_size(metrics.favorite)
                        .truncate(),
                    )
                })
                .inner
                .hint(
                    &with_key(l, &action, crate::favorite_key_name(slot)),
                    reason,
                );
            if response.clicked() {
                intents.push(favorite_intent(favorite));
            }
            response.context_menu(|ui| {
                draw_assign_menu(
                    ui,
                    (slot, duration_frames),
                    &stingers,
                    (assign_enabled, reason),
                    intents,
                );
                draw_favorite_t_bar_start(ui, (slot, favorite), start, t_bar_kind, intents);
            });
        }
    });
}

/// A favourite's `Start on T-bar` item: selects the favourite's effect as
/// the T-bar kind and starts it as a manual transition (Cut has none).
pub(crate) fn draw_favorite_t_bar_start(
    ui: &mut Ui,
    (slot, favorite): (u8, TransitionFavoriteStatus),
    (enabled, reason): (bool, &str),
    t_bar_kind: &mut ManualTransitionKind,
    intents: &mut Vec<StudioIntent>,
) {
    let Some(kind) = manual_kind_for(favorite.effect, favorite.duration_frames) else {
        return;
    };
    let l = l10n::locale(ui.ctx());
    let hover = l.format(
        Msg::FavoriteTBarHover,
        &[
            ("slot", Arg::from(u32::from(slot))),
            ("effect", Arg::Str(&effect_name(l, favorite.effect))),
        ],
    );
    if ui
        .add_enabled(enabled, egui::Button::new(l.text(Msg::FavoriteTBar)))
        .hint(&hover, reason)
        .clicked()
    {
        *t_bar_kind = kind;
        intents.push(StudioIntent::StartManualTransition { kind });
        ui.close();
    }
}

/// The MORE key: a menu taking each automatic mix without a key of its own.
fn draw_more_transitions(
    ui: &mut Ui,
    (key, width): (egui::Button<'static>, f32),
    (enabled, reason): (bool, &str),
    duration_frames: u32,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    egui::containers::menu::MenuButton::from_button(key.min_size(Vec2::new(width, 24.0)))
        .ui(ui, |ui| {
            for kind in MORE_KINDS {
                let label = l.text(mix_name(kind).name);
                if ui
                    .add_enabled(enabled, egui::Button::new(label))
                    .hint(
                        &l.format(Msg::AutoKindHover, &[("kind", Arg::Str(label))]),
                        reason,
                    )
                    .clicked()
                {
                    intents.push(StudioIntent::Transition {
                        kind,
                        duration_frames,
                    });
                    ui.close();
                }
            }
        })
        .0
        .hint(l.text(Msg::MoreTransitionsHover), reason);
}

/// The right-click menu of an overlay channel's transition key: every
/// kind of the family, the current one selected.
pub(crate) fn draw_overlay_transition_menu(
    ui: &mut Ui,
    (channel, current, duration_frames): (
        fm_protocol::WireOverlayChannelId,
        OverlayTransitionKind,
        u32,
    ),
    (enabled, reason): (bool, &str),
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    for kind in OverlayTransitionKind::ALL {
        let label = l.text(overlay_name(kind).name);
        if ui
            .add_enabled(enabled, egui::Button::new(label).selected(kind == current))
            .hint(
                &l.format(
                    Msg::OverlayTransitionPickHover,
                    &[("kind", Arg::Str(label))],
                ),
                reason,
            )
            .clicked()
        {
            intents.push(StudioIntent::SetOverlayTransition {
                channel,
                transition: kind,
                duration_frames,
            });
            ui.close();
        }
    }
}

/// The right-click menu of one favourite key: `Assign…` and every effect
/// at the shared duration.
pub(crate) fn draw_assign_menu(
    ui: &mut Ui,
    (slot, duration_frames): (u8, u32),
    stingers: &[WireStingerSlotId],
    (enabled, reason): (bool, &str),
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let slot_arg = || Arg::from(u32::from(slot));
    ui.menu_button(l.text(Msg::AssignFavorite), |ui| {
        let effects = core::iter::once(TransitionFavoriteEffect::Cut)
            .chain(AutoTransitionKind::ALL.map(TransitionFavoriteEffect::Mix))
            .chain(
                stingers
                    .iter()
                    .copied()
                    .map(TransitionFavoriteEffect::Stinger),
            );
        for effect in effects {
            let label = effect_name(l, effect);
            let hover = l.format(
                Msg::AssignFavoriteItemHover,
                &[("slot", slot_arg()), ("effect", Arg::Str(&label))],
            );
            if ui
                .add_enabled(enabled, egui::Button::new(label.as_str()))
                .hint(&hover, reason)
                .clicked()
            {
                intents.push(StudioIntent::AssignTransitionFavorite {
                    slot,
                    favorite: TransitionFavoriteStatus {
                        effect,
                        duration_frames,
                    },
                });
                ui.close();
            }
        }
    })
    .response
    .hint(
        &l.format(
            Msg::AssignFavoriteHover,
            &[
                ("slot", slot_arg()),
                ("frames", Arg::Str(&l10n::frames(l, duration_frames))),
            ],
        ),
        reason,
    );
}
/// The largest label size in `min..=max` at which `text` (strong
/// proportional) fits `width` on one line; `min` when none does.
fn fitted_label_size(ui: &Ui, text: &str, (min, max): (f32, f32), width: f32) -> f32 {
    let natural = ui
        .painter()
        .layout_no_wrap(
            text.to_owned(),
            egui::FontId::proportional(max),
            text_color(),
        )
        .size()
        .x;
    if natural <= width || natural <= 0.0 {
        max
    } else {
        // Text width scales with the font size; step down in half points.
        ((max * width / natural) * 2.0).floor().max(min * 2.0) / 2.0
    }
}

const T_BAR_HEIGHT: f32 = 148.0;

#[allow(clippy::too_many_lines)] // one panel section, top to bottom
pub(crate) fn draw_transition_column(
    ui: &mut Ui,
    shell: &mut StudioShell,
    state: &StudioUiState,
    manual: ManualTransitionAvailability,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let metrics = ColumnMetrics::new(ui.available_width());
    ui.set_width(metrics.width);
    // Rows sit 5 points apart so the column fits the Default layout's upper
    // row at 1080p without scrolling.
    ui.spacing_mut().item_spacing = Vec2::new(6.0, 5.0);
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
            ui.set_width(metrics.keys_width());
            draw_auto_transitions(ui, metrics, shell, state, intents);
        });
        ui.add_space(4.0);
        draw_t_bar(ui, desired, manual, shell.t_bar_kind, intents);
    });
    let session_reason = unavailable_reason(state, Capability::Transition);
    draw_favorites(
        ui,
        metrics,
        shell,
        (state, (manual.start, session_reason)),
        intents,
    );
    ui.separator();
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
    let stingers: Vec<WireStingerSlotId> = state
        .view
        .as_ref()
        .map(|view| {
            view.stingers
                .iter()
                .filter_map(|stinger| WireStingerSlotId::new(stinger.slot))
                .collect()
        })
        .unwrap_or_default();
    draw_manual_transition(
        ui,
        (desired, realized),
        (manual, &mut shell.layout.show_manual_details),
        (session_reason, manual_reason),
        ManualStart {
            kind: &mut shell.t_bar_kind,
            stingers: &stingers,
            duration_frames: shell.transition_duration_frames,
        },
        names,
        intents,
    );
}

/// The rule and Fade to Black block that close the transition column. A
/// short Transitions tile pins this block along its bottom edge instead of
/// letting it scroll away (see `StudioShell::draw_transitions_body`).
pub(crate) fn draw_fade_to_black_block(
    ui: &mut Ui,
    shell: &mut StudioShell,
    state: &StudioUiState,
    intents: &mut Vec<StudioIntent>,
) {
    ui.separator();
    fade_to_black::draw_fade_to_black(ui, &mut shell.fade_to_black_duration_frames, state, intents);
    shell.set_fade_to_black_duration_frames(shell.fade_to_black_duration_frames);
}

fn draw_auto_transitions(
    ui: &mut Ui,
    metrics: ColumnMetrics,
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
            metrics.button,
        )
    };
    let secondary =
        |label: &str| console_button(label, widget_color(), border_strong_color(), metrics.button);
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
                    metrics.button,
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
    draw_more_transitions(
        ui,
        (
            secondary(l.text(Msg::MoreTransitionsKey)),
            metrics.keys_width(),
        ),
        (availability.basic(), reason),
        duration_frames,
        intents,
    );
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
    start: ManualStart<'_>,
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
    let in_flight = matches!(desired, ManualTransitionStatus::Active(_))
        || matches!(realized, ManualTransitionStatus::Active(_));
    if !*details && in_flight {
        // With the details closed, the position and Commit/Cancel keys
        // take the start keys' row while a transition is in flight (the
        // start keys cannot act then), so the column keeps its height and
        // the console below does not jump.
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
        return;
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        draw_manual_start_buttons(
            ui,
            (availability.start, start_reason),
            availability.alpha_fade_start_exposed,
            start,
            intents,
        );
    });
    if !*details {
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
    /// The desired transition kind in the console language, or `None`
    /// while no manual transition is desired.
    pub(crate) kind: Option<String>,
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
            kind: Some(manual_kind_text(names.locale, state.kind)),
            route: Some((names.name(state.from), names.name(state.to))),
            percent: state.position.basis_points() / 100,
            confirmed,
        },
    }
}

/// A manual kind's key label (`WIPE`, `ALPHA FADE`, `STING 2`).
pub(crate) fn manual_kind_text(l: Locale, kind: ManualTransitionKind) -> String {
    match manual_effect(kind) {
        TransitionFavoriteEffect::Mix(AutoTransitionKind::AlphaFade) => {
            l.text(Msg::AlphaFadeKind).to_owned()
        }
        effect => effect_short(l, effect),
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
                    RichText::new(
                        summary
                            .kind
                            .as_deref()
                            .unwrap_or(l.text(Msg::NoManualTransition)),
                    )
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
/// Dragging an idle lever first starts the selected kind (`kind`); until
/// the daemon confirms the start, the rest of that drag only moves it.
#[allow(clippy::too_many_lines)] // one panel section, top to bottom
fn draw_t_bar(
    ui: &mut Ui,
    desired: ManualTransitionStatus,
    availability: ManualTransitionAvailability,
    kind: ManualTransitionKind,
    intents: &mut Vec<StudioIntent>,
) {
    const WIDTH: f32 = 40.0;
    const HANDLE: Vec2 = Vec2::new(36.0, 16.0);
    let enabled = availability.active_controls || availability.start;
    let started_id = egui::Id::new("studio-t-bar-started");
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
    let started = ui.data(|data| data.get_temp::<bool>(started_id).unwrap_or(false));
    if enabled
        && moved
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let fraction = (travel_bottom - pointer.y) / (travel_bottom - travel_top);
        let position = t_bar_position(fraction);
        let sent = if started && !availability.active_controls {
            // Started earlier in this drag; the start is not confirmed yet.
            vec![StudioIntent::SetManualTransitionPosition { position }]
        } else {
            t_bar_intents(
                desired,
                availability,
                TBarGesture::MoveTo { kind, position },
            )
        };
        if sent
            .iter()
            .any(|intent| matches!(intent, StudioIntent::StartManualTransition { .. }))
        {
            ui.data_mut(|data| data.insert_temp(started_id, true));
        }
        intents.extend(sent);
    }
    if !response.dragged() && !response.drag_started() {
        ui.data_mut(|data| data.remove::<bool>(started_id));
    }
    let response = response.on_hover_text(t_bar_hover(l10n::locale(ui.ctx()), availability, kind));
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
fn t_bar_hover(
    l: Locale,
    availability: ManualTransitionAvailability,
    kind: ManualTransitionKind,
) -> String {
    if availability.active_controls {
        l.format(
            Msg::TBarHoverActive,
            &[
                ("commit", Arg::Str(l.text(Msg::CommitKey))),
                ("cancel", Arg::Str(l.text(Msg::CancelKey))),
            ],
        )
    } else if availability.start {
        with_key(
            l,
            &l.format(
                Msg::TBarHoverReady,
                &[("kind", Arg::Str(&effect_name(l, manual_effect(kind))))],
            ),
            "T",
        )
    } else {
        l.format(
            Msg::TBarHoverIdle,
            &[(
                "keys",
                Arg::Str(&manual_start_keys(
                    l,
                    availability.alpha_fade_start_exposed,
                    ", ",
                )),
            )],
        )
    }
}

/// What the manual start keys start: the selected T-bar kind they set, the
/// configured Stinger slots MORE offers, and a Stinger's clip length.
pub(crate) struct ManualStart<'a> {
    pub(crate) kind: &'a mut ManualTransitionKind,
    pub(crate) stingers: &'a [WireStingerSlotId],
    pub(crate) duration_frames: u32,
}

fn draw_manual_start_buttons(
    ui: &mut Ui,
    (enabled, reason): (bool, &str),
    alpha_fade_exposed: bool,
    start: ManualStart<'_>,
    intents: &mut Vec<StudioIntent>,
) {
    let ManualStart {
        kind: selected,
        stingers,
        duration_frames,
    } = start;
    let l = l10n::locale(ui.ctx());
    let mut keys = vec![
        AutoTransitionKind::Fade,
        AutoTransitionKind::Wipe,
        AutoTransitionKind::Slide,
    ];
    if alpha_fade_exposed {
        keys.push(AutoTransitionKind::AlphaFade);
    }
    // Equal-width keys, MORE last, that always fill the column, whatever
    // the language.
    #[allow(clippy::cast_precision_loss)] // four or five keys
    let count = (keys.len() + 1) as f32;
    let spacing = ui.spacing().item_spacing.x;
    let size = Vec2::new(
        ((ui.available_width() - spacing * (count - 1.0)) / count).floor(),
        22.0,
    );
    ui.spacing_mut().button_padding.x = 2.0;
    // One label size for every key: the largest at which each label fits
    // its key, so a long word (Spanish `DESLIZAR`) shrinks instead of
    // ending in an ellipsis.
    let label_max = crate::theme::legible(10.0);
    let label_size = keys
        .iter()
        .map(|mix| l.text(mix_name(*mix).short))
        .chain([l.text(Msg::MoreTransitionsKey)])
        .map(|label| fitted_label_size(ui, label, (MANUAL_LABEL_MIN, label_max), size.x - 6.0))
        .fold(label_max, f32::min);
    for mix in &keys {
        let kind = ManualTransitionKind::from_mix(*mix);
        let TransitionName { short, name } = mix_name(*mix);
        let tooltip = l.format(Msg::ManualStartHover, &[("kind", Arg::Str(l.text(name)))]);
        let response = ui
            .add_enabled(
                enabled,
                start_button(l.text(short), size, label_size).selected(*selected == kind),
            )
            .hint(&tooltip, reason);
        if response.clicked() {
            *selected = kind;
            intents.push(StudioIntent::StartManualTransition { kind });
        }
    }
    // Every other mix and each configured Stinger, scrubbed by the lever.
    let more = AutoTransitionKind::ALL
        .into_iter()
        .filter(|mix| !keys.contains(mix))
        .map(TransitionFavoriteEffect::Mix)
        .chain(
            stingers
                .iter()
                .copied()
                .map(TransitionFavoriteEffect::Stinger),
        );
    let selected_more = !keys
        .iter()
        .any(|mix| *selected == ManualTransitionKind::from_mix(*mix));
    let key =
        start_button(l.text(Msg::MoreTransitionsKey), size, label_size).selected(selected_more);
    egui::containers::menu::MenuButton::from_button(key)
        .ui(ui, |ui| {
            for effect in more {
                let Some(kind) = manual_kind_for(effect, duration_frames) else {
                    continue;
                };
                let label = effect_name(l, effect);
                let tooltip = l.format(Msg::ManualStartHover, &[("kind", Arg::Str(&label))]);
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::new(label.as_str()).selected(*selected == kind),
                    )
                    .hint(&tooltip, reason)
                    .clicked()
                {
                    *selected = kind;
                    intents.push(StudioIntent::StartManualTransition { kind });
                    ui.close();
                }
            }
        })
        .0
        .hint(
            &l.format(
                Msg::ManualMoreHover,
                &[("frames", Arg::Str(&l10n::frames(l, duration_frames)))],
            ),
            reason,
        );
}

fn start_button(label: &str, size: Vec2, text_size: f32) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(label.to_owned())
            .size(text_size)
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
        // A fixed-width readout sized for the widest value, so Commit and
        // Cancel stay put while the position grows from `0 bp` to
        // `10000 bp`.
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let widest = ui
            .painter()
            .layout_no_wrap(
                format!("{} bp", ManualTransitionPosition::END.basis_points()),
                font,
                text_color(),
            )
            .size()
            .x;
        let readout = Vec2::new(
            widest + 2.0 * ui.spacing().button_padding.x + 4.0,
            ui.spacing().interact_size.y,
        );
        let response = ui
            .add_enabled_ui(enabled, |ui| {
                ui.add_sized(
                    readout,
                    DragValue::new(&mut position)
                        .range(
                            ManualTransitionPosition::START.basis_points()
                                ..=ManualTransitionPosition::END.basis_points(),
                        )
                        .suffix(" bp"),
                )
            })
            .inner
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
                ("kind", Arg::Str(&manual_kind_text(l, state.kind))),
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
    fn favourites_take_their_effect_and_overlay_kinds_cycle_the_family() {
        let defaults = TransitionFavoriteStatus::defaults();
        assert_eq!(
            defaults.map(favorite_intent),
            [
                AutoTransitionKind::Fade,
                AutoTransitionKind::Wipe,
                AutoTransitionKind::Fly,
                AutoTransitionKind::Cube,
            ]
            .map(|kind| StudioIntent::Transition {
                kind,
                duration_frames: 30,
            })
        );
        assert_eq!(
            favorite_intent(TransitionFavoriteStatus {
                effect: TransitionFavoriteEffect::Cut,
                duration_frames: 1,
            }),
            StudioIntent::Cut
        );
        let slot = WireStingerSlotId::new(3).unwrap();
        assert_eq!(
            favorite_intent(TransitionFavoriteStatus {
                effect: TransitionFavoriteEffect::Stinger(slot),
                duration_frames: 45,
            }),
            StudioIntent::Stinger {
                slot,
                duration_frames: 45,
            }
        );
        let mut kind = OverlayTransitionKind::Cut;
        let mut seen = Vec::new();
        for _ in 0..OverlayTransitionKind::ALL.len() {
            kind = next_overlay_transition(kind);
            seen.push(kind);
        }
        assert_eq!(seen.first(), Some(&OverlayTransitionKind::Fade));
        assert_eq!(seen.last(), Some(&OverlayTransitionKind::Cut));
        assert!(
            OverlayTransitionKind::ALL
                .iter()
                .all(|kind| seen.contains(kind))
        );
        // Every key has a label, and every mix is reachable from the column.
        for kind in AutoTransitionKind::ALL {
            assert_ne!(mix_name(kind).short.en(), "");
            assert_ne!(mix_name(kind).name.en(), "");
        }
        assert!(MORE_KINDS.iter().all(|kind| !matches!(
            kind,
            AutoTransitionKind::Fade
                | AutoTransitionKind::AlphaFade
                | AutoTransitionKind::Wipe
                | AutoTransitionKind::Slide
                | AutoTransitionKind::Zoom
        )));
        assert_eq!(MORE_KINDS.len() + 5, AutoTransitionKind::ALL.len());
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one table of every surface
    fn t_bar_gestures_send_the_same_commands_from_every_surface() {
        let idle = ManualTransitionStatus::Inactive;
        let ready = ManualTransitionAvailability {
            start: true,
            alpha_fade_start_exposed: true,
            active_controls: false,
        };
        let live = ManualTransitionAvailability {
            start: false,
            alpha_fade_start_exposed: true,
            active_controls: true,
        };
        let at = |basis_points| ManualTransitionPosition::new(basis_points).unwrap();
        let set = |basis_points| StudioIntent::SetManualTransitionPosition {
            position: at(basis_points),
        };
        let kind = ManualTransitionKind::CubeZoom;
        // Dragging an idle lever starts the selected kind, then moves it.
        assert_eq!(
            t_bar_intents(
                idle,
                ready,
                TBarGesture::MoveTo {
                    kind,
                    position: at(2_500)
                }
            ),
            vec![StudioIntent::StartManualTransition { kind }, set(2_500)]
        );
        assert_eq!(
            t_bar_intents(
                idle,
                ready,
                TBarGesture::MoveTo {
                    kind,
                    position: ManualTransitionPosition::START
                }
            ),
            vec![StudioIntent::StartManualTransition { kind }]
        );
        // An active lever moves, reverses, steps within range, and ends.
        let moving = active(4_750);
        let to = |basis_points| TBarGesture::MoveTo {
            kind,
            position: at(basis_points),
        };
        assert_eq!(t_bar_intents(moving, live, to(3_000)), vec![set(3_000)]);
        assert_eq!(t_bar_intents(moving, live, to(4_750)), Vec::new());
        assert_eq!(
            t_bar_intents(moving, live, TBarGesture::Step(1_000)),
            vec![set(5_750)]
        );
        assert_eq!(
            t_bar_intents(active(9_500), live, TBarGesture::Step(1_000)),
            vec![set(10_000)]
        );
        assert_eq!(
            t_bar_intents(active(0), live, TBarGesture::Step(-1_000)),
            Vec::new()
        );
        assert_eq!(
            t_bar_intents(moving, live, TBarGesture::Commit),
            vec![StudioIntent::CommitManualTransition]
        );
        assert_eq!(
            t_bar_intents(moving, live, TBarGesture::Cancel),
            vec![StudioIntent::CancelManualTransition]
        );
        // Nothing moves without the matching availability.
        assert_eq!(
            t_bar_intents(idle, ready, TBarGesture::Step(1_000)),
            Vec::new()
        );
        assert_eq!(t_bar_intents(idle, ready, TBarGesture::Commit), Vec::new());
        assert_eq!(
            t_bar_intents(moving, live, TBarGesture::Start(kind)),
            Vec::new()
        );
        // The console keys reduce to the same gestures.
        assert_eq!(
            t_bar_key_gesture(egui::Key::T, kind),
            Some(TBarGesture::Start(kind))
        );
        assert_eq!(
            t_bar_key_gesture(egui::Key::PageDown, kind),
            Some(TBarGesture::Step(-T_BAR_KEY_STEP_BASIS_POINTS))
        );
        assert_eq!(
            t_bar_key_gesture(egui::Key::End, kind),
            Some(TBarGesture::Commit)
        );
        assert_eq!(t_bar_key_gesture(egui::Key::A, kind), None);
        // Every effect but Cut has a manual form; a Stinger carries its clip.
        let slot = WireStingerSlotId::new(2).unwrap();
        assert_eq!(manual_kind_for(TransitionFavoriteEffect::Cut, 30), None);
        assert_eq!(
            manual_kind_for(TransitionFavoriteEffect::Stinger(slot), 45),
            Some(ManualTransitionKind::Stinger {
                slot,
                duration_frames: 45
            })
        );
        for mix in AutoTransitionKind::ALL {
            let kind = manual_kind_for(TransitionFavoriteEffect::Mix(mix), 30).unwrap();
            assert_eq!(manual_effect(kind), TransitionFavoriteEffect::Mix(mix));
        }
        assert_eq!(
            manual_kind_text(
                Locale::ENGLISH,
                ManualTransitionKind::Stinger {
                    slot,
                    duration_frames: 45
                }
            ),
            "STING 2"
        );
    }

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
        assert_eq!(summary.kind.as_deref(), Some("SLIDE"));
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
