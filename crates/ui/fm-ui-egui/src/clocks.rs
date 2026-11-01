//! View ▸ Production clocks (SW-013, Protocol 2.149/2.150).
//!
//! Lists every clock the show defines with its kind, state, and reading, and
//! lets an operator run a timer: start, pause, reset, set, adjust, and store
//! a preset. The readings arrive in `diagnostics_response`; the window asks
//! for them again about once a second while it is open, after a manual
//! Refresh, and every accepted control carries its clock's new reading.
//! Controls need transition authority, as the daemon's clock control does.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use egui::{Button, DragValue, RichText, Ui};
use fm_protocol::{
    ClockControlOperation, ProductionClockKind, ProductionClockState, ProductionClockStatus,
};

use crate::gating::{self, Hint};
use crate::l10n::{self, Msg};
use crate::theme::{Tone, card, muted_color, status_chip};
use crate::{StudioIntent, StudioShell, StudioUiState};

const DIALOG_WIDTH: f32 = 640.0;
/// How often the open window asks for fresh readings.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
/// The largest whole-second value the Set, Adjust, and Preset fields take.
/// It stays far inside the nanosecond range (`i64::MAX` is about 292 years).
const MAX_DRAFT_SECONDS: i64 = 86_400;

/// The window's own state: whether it is open, when readings were last
/// asked for, and one value draft per clock.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ClocksWindow {
    pub(crate) open: bool,
    /// When the window last asked for readings; `None` asks on the next
    /// frame, so opening or reopening always fetches.
    last_refresh: Option<Instant>,
    /// The seconds Set, Adjust, and Preset use, per clock name.
    drafts: BTreeMap<String, i64>,
}

impl StudioShell {
    /// Draws the Production clocks window while it is open.
    pub(crate) fn draw_clocks_window(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        if !self.clocks.open {
            return;
        }
        let now = Instant::now();
        if self
            .clocks
            .last_refresh
            .is_none_or(|at| now.duration_since(at) >= REFRESH_INTERVAL)
        {
            intents.push(StudioIntent::RequestClocks);
            self.clocks.last_refresh = Some(now);
        }
        // Readings tick while the window is open even if the rest of the
        // console is idle.
        context.request_repaint_after(REFRESH_INTERVAL);
        let l = self.locale;
        let close = crate::menu::help_modal(
            context,
            "studio-clocks",
            l.text(Msg::ClocksTitle),
            DIALOG_WIDTH,
            |ui| draw_contents(ui, &mut self.clocks, state, intents),
        );
        if close {
            self.clocks.open = false;
            // The next opening fetches at once.
            self.clocks.last_refresh = None;
        }
    }
}

fn draw_contents(
    ui: &mut Ui,
    window: &mut ClocksWindow,
    state: &StudioUiState,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    ui.spacing_mut().interact_size.y = 24.0;
    ui.label(
        RichText::new(l.text(Msg::ClocksIntro))
            .size(crate::theme::legible(11.0))
            .color(muted_color()),
    );
    if let Some(error) = &state.clocks_error {
        ui.add_space(4.0);
        ui.label(
            RichText::new(error)
                .size(crate::theme::legible(11.0))
                .color(Tone::Fault.color()),
        );
    }
    ui.add_space(6.0);
    if state.clocks.is_empty() {
        let empty_show = state
            .view
            .as_ref()
            .is_some_and(|view| view.production_clocks().is_empty());
        ui.label(
            RichText::new(l.text(if empty_show {
                Msg::ClocksEmpty
            } else {
                Msg::ClocksReasonNoReading
            }))
            .size(crate::theme::legible(11.0))
            .color(muted_color()),
        );
    } else {
        egui::ScrollArea::vertical()
            .id_salt("studio-clocks-scroll")
            .max_height(520.0)
            .show(ui, |ui| {
                for clock in &state.clocks {
                    card().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        draw_clock(ui, window, state, clock, intents);
                    });
                    ui.add_space(4.0);
                }
            });
    }
    ui.add_space(4.0);
    if ui
        .button(l.text(Msg::ClocksRefresh))
        .on_hover_text(l.text(Msg::ClocksRefreshHover))
        .clicked()
    {
        intents.push(StudioIntent::RequestClocks);
        window.last_refresh = Some(Instant::now());
    }
}

fn draw_clock(
    ui: &mut Ui,
    window: &mut ClocksWindow,
    state: &StudioUiState,
    clock: &ProductionClockStatus,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(&clock.name)
                .size(crate::theme::legible(13.0))
                .strong(),
        );
        status_chip(
            ui,
            l.text(kind_label(clock.kind)),
            Tone::Idle,
            chip_width(l.text(kind_label(clock.kind))),
        );
        status_chip(
            ui,
            l.text(state_label(clock.state)),
            state_tone(clock.state),
            chip_width(l.text(state_label(clock.state))),
        );
        if clock.expired {
            status_chip(
                ui,
                l.text(Msg::ClocksExpired),
                Tone::Fault,
                chip_width(l.text(Msg::ClocksExpired)),
            );
        }
        ui.with_layout(l10n::trailing_layout(l), |ui| {
            ui.label(
                RichText::new(&clock.text)
                    .monospace()
                    .size(crate::theme::legible(18.0))
                    .color(clock_color(clock)),
            );
        });
    });
    if let Some(zone) = &clock.zone {
        ui.label(
            RichText::new(zone)
                .size(crate::theme::legible(10.0))
                .color(muted_color()),
        );
    }
    if clock.kind != ProductionClockKind::Timer {
        ui.label(
            RichText::new(l.text(Msg::ClocksReasonNotTimer))
                .size(crate::theme::legible(10.0))
                .color(muted_color()),
        );
        return;
    }
    let blocked = clock_block_reason(state);
    ui.horizontal(|ui| {
        for (label, hover, operation) in [
            (
                Msg::ClocksStart,
                Msg::ClocksStartHover,
                ClockControlOperation::Start,
            ),
            (
                Msg::ClocksPause,
                Msg::ClocksPauseHover,
                ClockControlOperation::Pause,
            ),
            (
                Msg::ClocksReset,
                Msg::ClocksResetHover,
                ClockControlOperation::Reset,
            ),
        ] {
            if ui
                .add_enabled(blocked.is_none(), Button::new(l.text(label)))
                .hint(l.text(hover), blocked.unwrap_or(""))
                .clicked()
            {
                intents.push(StudioIntent::ClockControl {
                    name: clock.name.clone(),
                    operation,
                });
            }
        }
    });
    ui.horizontal(|ui| {
        let draft = window.drafts.entry(clock.name.clone()).or_insert(0);
        ui.add_enabled(
            blocked.is_none(),
            DragValue::new(&mut *draft)
                .range(-MAX_DRAFT_SECONDS..=MAX_DRAFT_SECONDS)
                .speed(1.0)
                .suffix(" s"),
        )
        .hint(l.text(Msg::ClocksSecondsHover), blocked.unwrap_or(""));
        for (label, hover, make) in [
            (
                Msg::ClocksSet,
                Msg::ClocksSetHover,
                ClockControlOperation::SetValue as fn(i64) -> ClockControlOperation,
            ),
            (
                Msg::ClocksAdjust,
                Msg::ClocksAdjustHover,
                ClockControlOperation::Adjust as fn(i64) -> ClockControlOperation,
            ),
            (
                Msg::ClocksPreset,
                Msg::ClocksPresetHover,
                ClockControlOperation::SetPreset as fn(i64) -> ClockControlOperation,
            ),
        ] {
            let operation = make(draft.saturating_mul(1_000_000_000));
            if ui
                .add_enabled(blocked.is_none(), Button::new(l.text(label)))
                .hint(l.text(hover), blocked.unwrap_or(""))
                .clicked()
            {
                intents.push(StudioIntent::ClockControl {
                    name: clock.name.clone(),
                    operation,
                });
            }
        }
    });
}

/// Why no timer key works now, in English (see [`l10n::disabled_text`]).
fn clock_block_reason(state: &StudioUiState) -> Option<&'static str> {
    if !state.connection_status.controls_enabled() || state.view.is_none() || !state.can_transition
    {
        return Some(gating::unavailable_reason(
            state,
            gating::Capability::Transition,
        ));
    }
    None
}

const fn kind_label(kind: ProductionClockKind) -> Msg {
    match kind {
        ProductionClockKind::Timer => Msg::ClocksKindTimer,
        ProductionClockKind::TimeOfDay => Msg::ClocksKindTimeOfDay,
        ProductionClockKind::MediaRemaining => Msg::ClocksKindMediaRemaining,
        ProductionClockKind::Stream => Msg::ClocksKindStream,
        ProductionClockKind::Recording => Msg::ClocksKindRecording,
        ProductionClockKind::Replay => Msg::ClocksKindReplay,
        ProductionClockKind::MultiCorder => Msg::ClocksKindMultiCorder,
        ProductionClockKind::External => Msg::ClocksKindExternal,
    }
}

const fn state_label(state: ProductionClockState) -> Msg {
    match state {
        ProductionClockState::Idle => Msg::ClocksStateIdle,
        ProductionClockState::Running => Msg::ClocksStateRunning,
        ProductionClockState::Paused => Msg::ClocksStatePaused,
        ProductionClockState::Expired => Msg::ClocksStateExpired,
        ProductionClockState::Stopped => Msg::ClocksStateStopped,
        ProductionClockState::Live => Msg::ClocksStateLive,
        ProductionClockState::Unavailable => Msg::ClocksStateUnavailable,
    }
}

const fn state_tone(state: ProductionClockState) -> Tone {
    match state {
        ProductionClockState::Running => Tone::Ready,
        ProductionClockState::Live => Tone::OnAir,
        ProductionClockState::Expired | ProductionClockState::Unavailable => Tone::Fault,
        ProductionClockState::Idle
        | ProductionClockState::Paused
        | ProductionClockState::Stopped => Tone::Idle,
    }
}

fn clock_color(clock: &ProductionClockStatus) -> egui::Color32 {
    if clock.expired {
        Tone::Fault.color()
    } else if clock.state == ProductionClockState::Live {
        Tone::OnAir.color()
    } else {
        crate::theme::text_color()
    }
}

/// A status chip wide enough for `text`.
fn chip_width(text: &str) -> f32 {
    crate::theme::legible(9.0).mul_add(text.chars().count() as f32 * 0.62, 14.0)
}
