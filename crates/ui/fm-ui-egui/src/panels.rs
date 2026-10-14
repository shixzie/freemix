//! Right-hand side panel: on-air outputs (recording and streams) above a
//! tabbed area for overlays and stingers.

use core::num::NonZeroU128;

use egui::{Button, Color32, RichText, ScrollArea, Stroke, Ui, Vec2};
use fm_protocol::{
    StingerReadiness, StreamRealizedState, StreamStatusMessage, StreamStatusSample,
    WireRecorderSlot, WireStingerSlotId,
};
use fm_ui_model::{StingerStatus, StreamStatus};

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::theme::{
    ACCENT, AMBER, BORDER, ERROR, GRAPHITE_RAISED, MUTED, PREVIEW, PROGRAM, PROGRAM_FILL, TEXT,
    TEXT_DIM, card, mono, pill, section_header, section_label,
};
use crate::{
    RecordingAction, StudioConnectionStatus, StudioIntent, StudioShell, StudioUiState,
    TransitionGate, overlays, recording_action, transition_availability,
};

/// Tabs of the lower side panel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SideTab {
    #[default]
    Overlays,
    Stingers,
}

pub(crate) const STREAMS_NONE_CONFIGURED: &str = "STREAMS | NONE CONFIGURED";

impl StudioShell {
    pub(crate) fn draw_side_panel(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_header(ui, "Recording", |_| {});
            if let Some(action) = self.draw_recording(ui, state) {
                intents.push(StudioIntent::Recording(action));
            }
            ui.add_space(4.0);
            draw_streams(ui, state);
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            for (tab, label) in [
                (SideTab::Overlays, "OVERLAYS"),
                (SideTab::Stingers, "STINGERS"),
            ] {
                let selected = self.side_tab == tab;
                let text = RichText::new(label)
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
                    draw_stingers(ui, state, self.transition_duration_frames, intents);
                }
            });
    }

    /// Draws the compact recording panel and returns the selected action.
    ///
    /// The panel exposes exactly one action for the authoritative desired flag
    /// and keeps it disabled while an earlier recording intent is unresolved.
    #[allow(clippy::unused_self)] // the panel reads immutable state; the shell receiver matches `draw_inputs`
    pub fn draw_recording(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
    ) -> Option<RecordingAction> {
        let mut selected = None;
        for (index, desired) in state.recording_desired.iter().copied().enumerate() {
            let Some(recorder) = u8::try_from(index + 1).ok().and_then(WireRecorderSlot::new)
            else {
                continue;
            };
            let slot = index + 1;
            let requested = if desired {
                RecordingAction::Stop(recorder)
            } else {
                RecordingAction::Start(recorder)
            };
            ui.horizontal(|ui| {
                ui.label(mono(format!("REC {slot}")).size(11.5).strong().color(TEXT));
                pill(
                    ui,
                    recording_status_label(desired),
                    if desired { PROGRAM } else { MUTED },
                    desired,
                );
                if state.recording_pending[index] {
                    pill(ui, "PENDING", AMBER, false);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (label, fill, stroke) = match requested {
                        RecordingAction::Start(_) => {
                            ("START", PROGRAM_FILL, PROGRAM.gamma_multiply(0.6))
                        }
                        RecordingAction::Stop(_) => {
                            ("STOP", crate::theme::WIDGET, crate::theme::BORDER_STRONG)
                        }
                    };
                    if ui
                        .add_enabled(
                            recording_action(state, requested).is_some(),
                            Button::new(RichText::new(label).size(11.5).strong())
                                .fill(fill)
                                .stroke(Stroke::new(1.0, stroke))
                                .min_size(Vec2::new(84.0, 24.0)),
                        )
                        .hint(
                            &match requested {
                                RecordingAction::Start(_) => {
                                    format!("Start configured program recorder {slot}")
                                }
                                RecordingAction::Stop(_) => {
                                    format!("Stop configured program recorder {slot}")
                                }
                            },
                            if state.recording_pending[index] {
                                "an earlier recording command is still pending"
                            } else {
                                unavailable_reason(state, Capability::Transition)
                            },
                        )
                        .clicked()
                    {
                        selected = recording_action(state, requested);
                    }
                });
            });
        }
        selected
    }
}

/// Operator-facing recording status derived only from the replicated desired
/// flag; it is never an optimistic local guess.
pub(crate) const fn recording_status_label(desired: bool) -> &'static str {
    if desired { "RECORDING" } else { "STOPPED" }
}

fn draw_streams(ui: &mut Ui, state: &StudioUiState) {
    section_header(ui, "Streams", |ui| {
        ui.label(
            mono(format!("{}", state.streams.len()))
                .size(10.5)
                .color(TEXT_DIM),
        );
    });
    if streams_show_none_configured(
        state.connection_status,
        state.view.is_some(),
        &state.streams,
    ) {
        ui.label(
            RichText::new(STREAMS_NONE_CONFIGURED)
                .size(10.5)
                .color(MUTED),
        );
        return;
    }
    if state.streams.is_empty() {
        ui.label(
            RichText::new("STREAM STATE UNAVAILABLE")
                .size(10.5)
                .color(MUTED),
        );
        return;
    }
    for stream in &state.streams {
        let sample = latest_stream_sample(state.stream_status.as_ref(), stream.target);
        let response = ui
            .horizontal(|ui| {
                ui.add(
                    egui::Label::new(RichText::new(&stream.name).size(12.0).color(TEXT)).truncate(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // The latest lossy sample wins; otherwise the replicated
                    // last-confirmed realized state is shown.
                    let realized = sample.map_or(stream.realized, |sample| sample.realized);
                    pill(
                        ui,
                        &stream_realized_name(realized).to_uppercase(),
                        stream_state_color(realized),
                        realized == StreamRealizedState::Live,
                    )
                    .on_hover_text("Realized transport state");
                    pill(
                        ui,
                        if stream.desired_running { "ON" } else { "OFF" },
                        if stream.desired_running {
                            ACCENT
                        } else {
                            MUTED
                        },
                        false,
                    )
                    .on_hover_text("Desired running state");
                });
            })
            .response;
        response.on_hover_text(stream_row_text(stream, sample));
        if let Some(sample) = sample {
            ui.label(
                mono(format!(
                    "{} B · {} enq · {} drop{}",
                    sample.muxed_bytes,
                    sample.enqueued_pairs,
                    sample.dropped_pairs,
                    sample
                        .failure
                        .as_deref()
                        .map_or_else(String::new, |failure| format!(" · {failure}"))
                ))
                .size(10.0)
                .color(MUTED),
            );
        }
    }
}

const fn stream_state_color(state: StreamRealizedState) -> Color32 {
    match state {
        StreamRealizedState::Live => PROGRAM,
        StreamRealizedState::Starting
        | StreamRealizedState::WaitingToReconnect
        | StreamRealizedState::Congested => AMBER,
        StreamRealizedState::Failed => ERROR,
        StreamRealizedState::Stopped | StreamRealizedState::Unavailable => MUTED,
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
            label: "STATE UNAVAILABLE",
            color: MUTED,
            ready: false,
        },
        StingerSlotState::Unconfigured => StingerSlotPresentation {
            label: "UNCONFIGURED",
            color: MUTED,
            ready: false,
        },
        StingerSlotState::Readiness(StingerReadiness::NotRequested) => StingerSlotPresentation {
            label: "NOT REQUESTED",
            color: AMBER,
            ready: false,
        },
        StingerSlotState::Readiness(StingerReadiness::Ready) => StingerSlotPresentation {
            label: "READY",
            color: PREVIEW,
            ready: true,
        },
        StingerSlotState::Readiness(StingerReadiness::Missing) => StingerSlotPresentation {
            label: "MISSING",
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

fn draw_stingers(
    ui: &mut Ui,
    state: &StudioUiState,
    duration_frames: u32,
    intents: &mut Vec<StudioIntent>,
) {
    let availability = transition_availability(TransitionGate::from_state(state));
    let reason = unavailable_reason(state, Capability::Transition);
    ui.label(
        RichText::new("Fire a configured stinger from Preview to Program.")
            .size(10.5)
            .color(MUTED),
    );
    ui.add_space(4.0);
    let stingers = state.view.as_ref().map(|view| view.stingers.as_slice());
    let width = ((ui.available_width() - 8.0) / 2.0).max(80.0);
    for row in [[1_u8, 2], [3, 4], [5, 6], [7, 8]] {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for slot in row {
                let presentation = stinger_slot_presentation(stinger_slot_state(stingers, slot));
                if stinger_button(
                    ui,
                    (availability.stinger(), reason),
                    slot,
                    presentation,
                    width,
                ) {
                    intents.push(StudioIntent::Stinger {
                        slot: WireStingerSlotId::new(slot)
                            .expect("Studio renders only Stinger slots 1 through 8"),
                        duration_frames,
                    });
                }
            }
        });
    }
    ui.add_space(6.0);
    ui.label(section_label("Duration"));
    ui.label(
        mono(format!(
            "{duration_frames} fr (shared with automatic transitions)"
        ))
        .size(10.5)
        .color(TEXT_DIM),
    );
}

fn stinger_button(
    ui: &mut Ui,
    (enabled, reason): (bool, &str),
    slot: u8,
    presentation: StingerSlotPresentation,
    width: f32,
) -> bool {
    ui.add_enabled(
        enabled && presentation.ready,
        Button::new(
            RichText::new(format!("S{slot}\n{}", presentation.label))
                .size(11.0)
                .strong()
                .color(presentation.color),
        )
        .fill(GRAPHITE_RAISED)
        .stroke(Stroke::new(1.0, presentation.color.gamma_multiply(0.5)))
        .min_size(Vec2::new(width, 44.0)),
    )
    .hint(
        &format!(
            "Fire Stinger slot {slot} from Preview to Program: {}",
            presentation.label
        ),
        if enabled {
            "this Stinger slot is not configured and ready"
        } else {
            reason
        },
    )
    .clicked()
}
