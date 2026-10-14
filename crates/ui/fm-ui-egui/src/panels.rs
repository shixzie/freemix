//! Right-hand side panel: on-air outputs (recording and streams) above a
//! tabbed area for overlays and stingers.

use core::fmt::Write as _;
use core::num::NonZeroU128;

use egui::{
    Align2, Button, Color32, CornerRadius, FontFamily, FontId, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, Ui, Vec2, pos2,
};
use fm_protocol::{
    StingerReadiness, StreamRealizedState, StreamStatusMessage, StreamStatusSample,
    WireStingerSlotId,
};
use fm_ui_model::{StingerStatus, StreamStatus};

use crate::gating::{Capability, unavailable_reason};
use crate::l10n::{self, Arg, Msg, frames};
use crate::stinger_config::{self, StingerEditor};
use crate::theme::{
    ACCENT, AMBER, BORDER, BORDER_STRONG, ERROR, GRAPHITE, GRAPHITE_RAISED, MUTED, PREVIEW, TEXT,
    TEXT_DIM, Tone, WIDGET_HOVER, caption, card, elided, mono, paint_lamp, section_label,
};
use crate::{
    RecordingAction, StudioConnectionStatus, StudioIntent, StudioShell, StudioUiState,
    TransitionGate, outputs, overlays, transition_availability,
};

/// Tabs of the lower side panel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SideTab {
    #[default]
    Overlays,
    Stingers,
}

pub(crate) const STREAMS_NONE_CONFIGURED: &str = Msg::StreamsNoneConfigured.en();

impl StudioShell {
    pub(crate) fn draw_side_panel(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            if let Some(action) = self.draw_recording(ui, state) {
                intents.push(StudioIntent::Recording(action));
            }
            ui.add_space(6.0);
            outputs::draw_streams(ui, state, intents);
        });
        ui.add_space(8.0);
        let l = self.locale;
        ui.horizontal(|ui| {
            for (tab, label) in [
                (SideTab::Overlays, Msg::TabOverlays),
                (SideTab::Stingers, Msg::TabStingers),
            ] {
                let selected = self.side_tab == tab;
                let text = RichText::new(l.text(label).to_uppercase())
                    .size(11.0)
                    .strong()
                    .extra_letter_spacing(1.0)
                    .color(if selected { TEXT } else { TEXT_DIM });
                let response = ui.add(
                    Button::new(text)
                        .fill(if selected {
                            GRAPHITE_RAISED
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(Stroke::new(
                            1.0,
                            if selected {
                                BORDER
                            } else {
                                Color32::TRANSPARENT
                            },
                        ))
                        .min_size(Vec2::new(96.0, 26.0)),
                );
                if selected {
                    let rect = response.rect;
                    ui.painter().line_segment(
                        [rect.left_bottom(), rect.right_bottom()],
                        Stroke::new(2.0, ACCENT),
                    );
                }
                if response.clicked() {
                    self.side_tab = tab;
                }
            }
        });
        ui.add_space(4.0);
        ScrollArea::vertical()
            .id_salt("studio-side-tabs")
            .auto_shrink([false, false])
            .show(ui, |ui| match self.side_tab {
                SideTab::Overlays => {
                    overlays::draw_overlays(ui, state, self.transition_duration_frames, intents);
                }
                SideTab::Stingers => {
                    draw_stingers(
                        ui,
                        state,
                        &mut self.stinger_editor,
                        self.transition_duration_frames,
                        intents,
                    );
                    stinger_config::draw_stinger_configuration(
                        ui,
                        state,
                        &mut self.stinger_editor,
                        self.transition_duration_frames,
                        intents,
                    );
                }
            });
    }

    /// Draws the recorder status rows and returns the selected action.
    ///
    /// Each row exposes exactly one action for the authoritative desired flag
    /// and keeps it disabled while an earlier recording intent is unresolved.
    #[allow(clippy::unused_self)] // the panel reads immutable state; the shell receiver matches `draw_inputs`
    pub fn draw_recording(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
    ) -> Option<RecordingAction> {
        outputs::draw_recorders(ui, state)
    }
}

/// Operator-facing recording status derived only from the replicated desired
/// flag; it is never an optimistic local guess.
pub(crate) const fn recording_status_label(desired: bool) -> &'static str {
    if desired {
        Msg::Recording.en()
    } else {
        Msg::Stopped.en()
    }
}

/// Pure placeholder decision: connected with a view but no configured streams.
pub(crate) const fn streams_show_none_configured(
    connection_status: StudioConnectionStatus,
    has_view: bool,
    streams: &[StreamStatus],
) -> bool {
    matches!(connection_status, StudioConnectionStatus::Ready) && has_view && streams.is_empty()
}

/// Finds the latest lossy status sample for one roster target, if any.
pub(crate) fn latest_stream_sample(
    status: Option<&StreamStatusMessage>,
    target: NonZeroU128,
) -> Option<&StreamStatusSample> {
    status.and_then(|message| {
        message
            .samples
            .iter()
            .find(|sample| sample.target.get() == target)
    })
}

/// Lowercase realized-state label matching the operator-facing protocol text.
pub(crate) const fn stream_realized_name(state: StreamRealizedState) -> &'static str {
    match state {
        StreamRealizedState::Stopped => "stopped",
        StreamRealizedState::Starting => "starting",
        StreamRealizedState::Live => "live",
        StreamRealizedState::WaitingToReconnect => "waiting-to-reconnect",
        StreamRealizedState::Congested => "congested",
        StreamRealizedState::Failed => "failed",
        StreamRealizedState::Unavailable => "unavailable",
    }
}

/// One compact roster row: canonical name plus desired flag and, when a latest
/// sample exists for that target, the retained realized transport counters and
/// any local HLS output health. Roster entries without samples show the desired
/// flag only; samples without roster entries are ignored by the caller.
pub(crate) fn stream_row_text(
    stream: &StreamStatus,
    sample: Option<&StreamStatusSample>,
) -> String {
    use core::fmt::Write as _;
    let mut row = format!("{} desired={}", stream.name, stream.desired_running);
    let Some(sample) = sample else {
        return row;
    };
    let _ = write!(
        row,
        " realized={} connected={} muxed_bytes={} enqueued_pairs={} dropped_pairs={} failure={}",
        stream_realized_name(sample.realized),
        sample.connected,
        sample.muxed_bytes,
        sample.enqueued_pairs,
        sample.dropped_pairs,
        sample.failure.as_deref().unwrap_or("none"),
    );
    if let Some(local) = sample.local {
        let _ = write!(
            row,
            " local={}seg/{}ms/{}B",
            local.segments, local.target_duration_ms, local.free_bytes
        );
    }
    row
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StingerSlotPresentation {
    pub(crate) label: &'static str,
    pub(crate) color: Color32,
    pub(crate) ready: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StingerSlotState {
    Unavailable,
    Unconfigured,
    Readiness(StingerReadiness),
}

pub(crate) const fn stinger_slot_presentation(state: StingerSlotState) -> StingerSlotPresentation {
    match state {
        StingerSlotState::Unavailable => StingerSlotPresentation {
            label: Msg::StingerStateUnavailable.en(),
            color: MUTED,
            ready: false,
        },
        StingerSlotState::Unconfigured => StingerSlotPresentation {
            label: Msg::StingerUnconfigured.en(),
            color: MUTED,
            ready: false,
        },
        StingerSlotState::Readiness(StingerReadiness::NotRequested) => StingerSlotPresentation {
            label: Msg::StingerNotRequested.en(),
            color: AMBER,
            ready: false,
        },
        StingerSlotState::Readiness(StingerReadiness::Ready) => StingerSlotPresentation {
            label: Msg::StingerReady.en(),
            color: PREVIEW,
            ready: true,
        },
        StingerSlotState::Readiness(StingerReadiness::Missing) => StingerSlotPresentation {
            label: Msg::StingerMissing.en(),
            color: ERROR,
            ready: false,
        },
    }
}

pub(crate) fn stinger_slot_state(stingers: Option<&[StingerStatus]>, slot: u8) -> StingerSlotState {
    let Some(stingers) = stingers else {
        return StingerSlotState::Unavailable;
    };
    stingers
        .iter()
        .find(|status| status.slot == slot)
        .map_or(StingerSlotState::Unconfigured, |status| {
            StingerSlotState::Readiness(status.readiness)
        })
}

/// Lamp tone of one Stinger slot state.
pub(crate) const fn stinger_slot_tone(state: StingerSlotState) -> Tone {
    match state {
        StingerSlotState::Unavailable | StingerSlotState::Unconfigured => Tone::Idle,
        StingerSlotState::Readiness(StingerReadiness::Ready) => Tone::Ready,
        StingerSlotState::Readiness(StingerReadiness::NotRequested) => Tone::Pending,
        StingerSlotState::Readiness(StingerReadiness::Missing) => Tone::Fault,
    }
}

/// What a click on one Stinger slot card does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StingerCardClick {
    /// Fire the slot from Preview to Program.
    Fire,
    /// Select the slot in the Configure section below the grid.
    Configure,
}

/// A ready slot fires when the transition gates allow it; any other click
/// only selects the slot for configuration, which is local and harmless.
pub(crate) const fn stinger_card_click(ready: bool, can_fire: bool) -> StingerCardClick {
    if ready && can_fire {
        StingerCardClick::Fire
    } else {
        StingerCardClick::Configure
    }
}

const STINGER_CARD_HEIGHT: f32 = 50.0;

fn draw_stingers(
    ui: &mut Ui,
    state: &StudioUiState,
    editor: &mut StingerEditor,
    duration_frames: u32,
    intents: &mut Vec<StudioIntent>,
) {
    let can_fire = transition_availability(TransitionGate::from_state(state)).stinger();
    let reason = unavailable_reason(state, Capability::Transition);
    let l = l10n::locale(ui.ctx());
    ui.add(egui::Label::new(caption(l.text(Msg::StingersIntro))).wrap());
    ui.add_space(2.0);
    let stingers = state.view.as_ref().map(|view| view.stingers.as_slice());
    let gap = 6.0;
    let width = ((ui.available_width() - gap) / 2.0).floor().max(80.0);
    for row in [[1_u8, 2], [3, 4], [5, 6], [7, 8]] {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for slot in row {
                let slot_state = stinger_slot_state(stingers, slot);
                let media = state.view.as_ref().and_then(|view| {
                    stinger_config::configured_stinger(view, slot)
                        .map(|status| crate::monitors::label_for_input(view, status.media_input))
                });
                let card = StingerCard {
                    slot,
                    state: slot_state,
                    media: media.as_deref(),
                    selected: editor.slot == slot,
                    can_fire,
                    reason,
                    width,
                };
                match draw_stinger_card(ui, &card) {
                    Some(StingerCardClick::Fire) => intents.push(StudioIntent::Stinger {
                        slot: WireStingerSlotId::new(slot)
                            .expect("Studio renders only Stinger slots 1 through 8"),
                        duration_frames,
                    }),
                    Some(StingerCardClick::Configure) if editor.slot != slot => {
                        editor.slot = slot;
                        editor.media = None;
                    }
                    Some(StingerCardClick::Configure) | None => {}
                }
            }
        });
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(section_label(l.text(Msg::Duration)));
        ui.label(mono(frames(l, duration_frames)).size(11.5).color(TEXT));
        ui.add(egui::Label::new(caption(l.text(Msg::StingerDurationShared))).truncate());
    });
}

struct StingerCard<'a> {
    slot: u8,
    state: StingerSlotState,
    media: Option<&'a str>,
    selected: bool,
    can_fire: bool,
    reason: &'a str,
    width: f32,
}

/// One fixed-size Stinger slot card; returns the click meaning, if clicked.
fn draw_stinger_card(ui: &mut Ui, card: &StingerCard<'_>) -> Option<StingerCardClick> {
    let l = l10n::locale(ui.ctx());
    let presentation = stinger_slot_presentation(card.state);
    let tone = stinger_slot_tone(card.state);
    let click = stinger_card_click(presentation.ready, card.can_fire);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(card.width, STINGER_CARD_HEIGHT), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fires = click == StingerCardClick::Fire;
        let hovered = response.hovered();
        let fill = if fires && response.is_pointer_button_down_on() {
            PREVIEW.gamma_multiply(0.25)
        } else if hovered {
            WIDGET_HOVER
        } else if fires {
            PREVIEW.gamma_multiply(0.10)
        } else {
            GRAPHITE
        };
        painter.rect_filled(rect, CornerRadius::same(4), fill);
        let stroke = if response.has_focus() {
            Stroke::new(2.0, ACCENT)
        } else if card.selected {
            Stroke::new(1.5, ACCENT.gamma_multiply(0.8))
        } else if tone == Tone::Idle {
            Stroke::new(1.0, if hovered { BORDER_STRONG } else { BORDER })
        } else {
            Stroke::new(
                1.0,
                tone.color()
                    .gamma_multiply(if hovered { 0.9 } else { 0.55 }),
            )
        };
        painter.rect_stroke(rect, CornerRadius::same(4), stroke, StrokeKind::Inside);
        painter.text(
            pos2(rect.min.x + 9.0, rect.min.y + 13.0),
            Align2::LEFT_CENTER,
            format!("S{}", card.slot),
            FontId::new(13.0, FontFamily::Monospace),
            if tone == Tone::Idle { TEXT_DIM } else { TEXT },
        );
        let label = elided(
            painter,
            l.tr(presentation.label),
            FontId::new(9.5, FontFamily::Proportional),
            tone.color(),
            rect.width() - 60.0,
        );
        let label_pos = pos2(
            rect.max.x - 9.0 - label.size().x,
            rect.min.y + 13.0 - label.size().y / 2.0,
        );
        paint_lamp(
            painter,
            pos2(label_pos.x - 9.0, rect.min.y + 13.0),
            3.5,
            tone,
        );
        painter.galley(label_pos, label, tone.color());
        let (media, color) = match (card.media, card.state) {
            (Some(media), _) => (media, TEXT_DIM),
            (None, StingerSlotState::Unavailable) => (l.text(Msg::StingerStateNotSynced), MUTED),
            (None, _) => (l.text(Msg::StingerEmptySlot), MUTED),
        };
        let media = elided(
            painter,
            media,
            FontId::new(11.0, FontFamily::Proportional),
            color,
            rect.width() - 18.0,
        );
        painter.galley(pos2(rect.min.x + 9.0, rect.max.y - 20.0), media, color);
    }
    response
        .on_hover_text(stinger_card_tooltip(card, presentation, click, l))
        .clicked()
        .then_some(click)
}

/// The card's tooltip: slot and readiness, media, and what a click does.
fn stinger_card_tooltip(
    card: &StingerCard<'_>,
    presentation: StingerSlotPresentation,
    click: StingerCardClick,
    l: crate::l10n::Locale,
) -> String {
    let mut tooltip = l.format(
        Msg::StingerCardTitle,
        &[
            ("slot", Arg::Plain(u64::from(card.slot))),
            ("state", Arg::Str(l.tr(presentation.label))),
        ],
    );
    if let Some(media) = card.media {
        let _ = write!(
            tooltip,
            "\n{}",
            l.format(Msg::StingerCardMedia, &[("name", Arg::Str(media))])
        );
    }
    match click {
        StingerCardClick::Fire => {
            let _ = write!(tooltip, "\n{}", l.text(Msg::StingerCardFire));
        }
        StingerCardClick::Configure if presentation.ready => {
            let _ = write!(
                tooltip,
                "\n{}\n{}",
                l.format(
                    Msg::StingerCardCannotFire,
                    &[("reason", Arg::Str(l.tr(card.reason)))]
                ),
                l.text(Msg::StingerCardConfigure)
            );
        }
        StingerCardClick::Configure => {
            let _ = write!(
                tooltip,
                "\n{} · {}",
                l.text(Msg::StingerCardNotReady),
                l.text(Msg::StingerCardConfigure)
            );
        }
    }
    tooltip
}
