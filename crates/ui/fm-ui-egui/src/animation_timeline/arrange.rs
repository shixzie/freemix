//! Header controls that arrange the timeline: the snapping toggle and the
//! Stagger menu, which cascades whole element animations in one undo step
//! (a number of frames apart, or spread to fill the phase). While the
//! pointer rests on the menu's Apply, the rows show the result ghosted.

use egui::containers::menu::{MenuButton, MenuConfig};
use egui::{DragValue, PopupCloseBehavior, Rect, RichText, Stroke, Ui, Vec2};
use fm_ui_model::animation_editor::{
    AnimDocument, MAX_STAGGER_FRAMES, StaggerOrder, StaggerSpacing,
};
use fm_ui_model::i18n::{Arg, Locale, Msg};

use super::AnimationTimelineState;
use crate::gating::Hint;
use crate::l10n;
use crate::theme::{
    ACCENT_FILL, accent_color, border_strong_color, field_label, legible, line_width, muted_color,
    text_color, widget_color,
};

/// Where the arrange controls were drawn, for callers and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ArrangeRects {
    pub snap: Option<Rect>,
    pub stagger: Option<Rect>,
    /// The Stagger menu's Apply button while the menu is open.
    pub stagger_apply: Option<Rect>,
}

/// The snapping toggle and the Stagger menu.
pub(super) fn draw<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) -> ArrangeRects {
    let idle = document.drag().is_none();
    let busy = l.text(Msg::AnimReasonDrag);
    let snap = snap_toggle(ui, l, document, state, idle, busy);
    let (stagger, stagger_apply) = stagger_menu(ui, l, document, state);
    ArrangeRects {
        snap: Some(snap),
        stagger: Some(stagger),
        stagger_apply,
    }
}

fn toggle_button(label: &str, on: bool) -> egui::Button<'static> {
    egui::Button::new(RichText::new(label).size(legible(11.0)).color(text_color()))
        .fill(if on { ACCENT_FILL } else { widget_color() })
        .stroke(Stroke::new(
            line_width(),
            if on {
                accent_color().gamma_multiply(0.7)
            } else {
                border_strong_color()
            },
        ))
        .min_size(Vec2::new(0.0, 20.0))
}

fn snap_toggle<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
    idle: bool,
    busy: &str,
) -> Rect {
    let on = document.view().snapping();
    let hover = l.text(if on {
        Msg::AnimSnapOnHover
    } else {
        Msg::AnimSnapOffHover
    });
    let response = ui
        .add_enabled(idle, toggle_button(l.text(Msg::AnimSnap), on))
        .hint_in(l, hover, busy);
    if response.clicked() {
        state.snapping = !on;
        document.view_mut().set_snapping(!on);
    }
    response.rect
}

/// Why staggering is unavailable now, `None` when it may run.
pub fn stagger_reason<T: Copy + Ord>(
    l: Locale,
    document: &AnimDocument<T>,
    state: &AnimationTimelineState<T>,
) -> Option<&'static str> {
    if let Some(reason) = state.lock_reason {
        return Some(l.tr(reason));
    }
    if document.drag().is_some() {
        return Some(l.text(Msg::AnimReasonDrag));
    }
    (document.stagger_targets().len() < 2).then(|| l.text(Msg::AnimReasonTooFewTargets))
}

fn stagger_menu<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) -> (Rect, Option<Rect>) {
    let reason = stagger_reason(l, document, state);
    let button = crate::theme::small_button(l.text(Msg::AnimStagger));
    let (response, menu) = ui
        .add_enabled_ui(reason.is_none(), |ui| {
            MenuButton::from_button(button)
                .config(MenuConfig::new().close_behavior(PopupCloseBehavior::CloseOnClickOutside))
                .ui(ui, |ui| stagger_contents(ui, l, document, state))
        })
        .inner;
    let rect = response.rect;
    response.hint_in(l, l.text(Msg::AnimStaggerHover), reason.unwrap_or(""));
    (rect, menu.map(|menu| menu.inner))
}

fn stagger_contents<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) -> Rect {
    ui.set_min_width(240.0);
    stagger_options(ui, l, state);
    stagger_apply(ui, l, document, state)
}

/// The spacing (frames apart, or spread to fill the phase) and the order.
fn stagger_options<T>(ui: &mut Ui, l: Locale, state: &mut AnimationTimelineState<T>) {
    ui.horizontal(|ui| {
        ui.label(field_label(l.text(Msg::AnimStaggerStep)));
        ui.add_enabled(
            !state.stagger_spread,
            DragValue::new(&mut state.stagger_frames)
                .range(0..=MAX_STAGGER_FRAMES)
                .speed(0.25)
                .suffix(" fr"),
        )
        .hint_in(
            l,
            l.text(Msg::AnimStaggerStepHover),
            l.text(Msg::AnimReasonSpreadSpacing),
        );
    });
    if ui
        .add(toggle_button(
            l.text(Msg::AnimStaggerSpread),
            state.stagger_spread,
        ))
        .on_hover_text(l.text(Msg::AnimStaggerSpreadHover))
        .clicked()
    {
        state.stagger_spread = !state.stagger_spread;
    }
    ui.horizontal(|ui| {
        for (order, label, hover) in [
            (
                StaggerOrder::TopFirst,
                Msg::AnimStaggerTopFirst,
                Msg::AnimStaggerTopFirstHover,
            ),
            (
                StaggerOrder::BottomFirst,
                Msg::AnimStaggerBottomFirst,
                Msg::AnimStaggerBottomFirstHover,
            ),
        ] {
            let on = state.stagger_order == order;
            if ui
                .add(toggle_button(l.text(label), on))
                .on_hover_text(l.text(hover))
                .clicked()
            {
                state.stagger_order = order;
            }
        }
    });
}

/// The elements in start order and Apply, whose hover previews the result.
fn stagger_apply<T: Copy + Ord>(
    ui: &mut Ui,
    l: Locale,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) -> Rect {
    let mut names: Vec<String> = document
        .stagger_targets()
        .into_iter()
        .map(|target| super::tracks::target_name(l, document, target))
        .collect();
    if state.stagger_order == StaggerOrder::BottomFirst {
        names.reverse();
    }
    let count = u32::try_from(names.len()).unwrap_or(u32::MAX);
    ui.label(
        RichText::new(names.join(" · "))
            .size(legible(11.0))
            .color(muted_color()),
    )
    .on_hover_text(l.text(Msg::AnimStaggerOrderHover));
    let reason = stagger_reason(l, document, state);
    let hover = if state.stagger_spread {
        l.format(
            Msg::AnimStaggerSpreadApplyHover,
            &[
                ("count", Arg::from(count)),
                (
                    "length",
                    Arg::Str(&l10n::frames(l, document.phase_frames())),
                ),
            ],
        )
    } else {
        l.format(
            Msg::AnimStaggerApplyHover,
            &[
                ("count", Arg::from(count)),
                ("step", Arg::Str(&l10n::frames(l, state.stagger_frames))),
            ],
        )
    };
    let apply = ui
        .add_enabled(
            reason.is_none(),
            crate::theme::small_button(l.text(Msg::AnimStaggerApply)),
        )
        .hint_in(l, &hover, reason.unwrap_or(""));
    let spacing = state.stagger_spacing();
    if apply.hovered() && reason.is_none() {
        // A refused plan (a phase past the limit) shows nothing; a click
        // then says why.
        state.stagger_preview = document.plan_stagger(spacing, state.stagger_order).ok();
    }
    if apply.clicked() {
        match document.stagger(spacing, state.stagger_order) {
            Ok(()) => state.notice = None,
            Err(error) => state.show_error(&error),
        }
        state.stagger_preview = None;
        ui.close();
    }
    apply.rect
}

impl<T> AnimationTimelineState<T> {
    /// The spacing the Stagger menu would apply.
    #[must_use]
    pub const fn stagger_spacing(&self) -> StaggerSpacing {
        if self.stagger_spread {
            StaggerSpacing::FillPhase
        } else {
            StaggerSpacing::Frames(self.stagger_frames)
        }
    }
}
