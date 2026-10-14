//! The on-air outputs card at the top of the side panel: one fixed-height
//! status row per recorder slot and per configured stream target.
//!
//! Every row is a lamp, a name, a one-line detail, a status chip, and exactly
//! one Start/Stop key. Rows are derived only from replicated state: the
//! recorder's authoritative desired flag (the protocol reports no realized
//! recorder telemetry), and each stream target's desired flag plus its latest
//! lossy transport sample or last-confirmed realized state.

use core::fmt::Write as _;

use egui::{
    Button, CornerRadius, FontFamily, FontId, Rect, RichText, Sense, Stroke, Ui, Vec2, pos2,
};
use fm_protocol::{StreamRealizedState, StreamStatusSample, WireStreamTargetId};
use fm_ui_model::StreamStatus;

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{Arg, Locale, Msg};
use crate::panels::{STREAMS_NONE_CONFIGURED, latest_stream_sample, stream_row_text};
use crate::stream_control::{stream_action_text, stream_unavailable_reason};
use crate::theme::{
    BORDER, BORDER_STRONG, GRAPHITE, MUTED, PROGRAM, PROGRAM_FILL, TEXT, TEXT_DIM, Tone, WIDGET,
    elided, mono, paint_lamp, section_header, status_chip_painted,
};
use crate::{
    RecordingAction, StreamAction, StudioIntent, StudioUiState, recording_action, stream_action,
    stream_command_pending,
};

/// Height of one output row; rows never change height with their state.
pub(crate) const OUTPUT_ROW_HEIGHT: f32 = 42.0;
const CHIP_WIDTH: f32 = 78.0;
const KEY_WIDTH: f32 = 58.0;

/// Operator presentation of one output row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OutputRow {
    /// Short uppercase status for the chip.
    pub(crate) status: &'static str,
    /// Lamp and chip tone.
    pub(crate) tone: Tone,
    /// One-line secondary detail under the name.
    pub(crate) detail: String,
}

/// Presents one recorder slot from its replicated desired flag.
pub(crate) fn recorder_row(l: Locale, has_view: bool, desired: bool, pending: bool) -> OutputRow {
    if !has_view {
        return OutputRow {
            status: Msg::StatusNoState.en(),
            tone: Tone::Idle,
            detail: l.text(Msg::RowNotSynchronized).to_owned(),
        };
    }
    let status = crate::panels::recording_status_label(desired);
    if pending {
        return OutputRow {
            status,
            tone: Tone::Pending,
            detail: l.text(Msg::RowCommandPending).to_owned(),
        };
    }
    if desired {
        OutputRow {
            status,
            tone: Tone::OnAir,
            detail: l.text(Msg::RowRecordingProgram).to_owned(),
        }
    } else {
        OutputRow {
            status,
            tone: Tone::Idle,
            detail: l.text(Msg::RowProgramIdle).to_owned(),
        }
    }
}

/// Presents one stream target from its desired flag and realized state.
///
/// The latest lossy sample wins over the replicated last-confirmed realized
/// state. A target that is desired running but not reported live is amber,
/// never red: red means the daemon reports the target live.
pub(crate) fn stream_row(
    l: Locale,
    stream: &StreamStatus,
    sample: Option<&StreamStatusSample>,
    pending: bool,
) -> OutputRow {
    let realized = sample.map_or(stream.realized, |sample| sample.realized);
    let desired = stream.desired_running;
    let (status, tone) = match realized {
        StreamRealizedState::Live if desired => (Msg::Live.en(), Tone::OnAir),
        StreamRealizedState::Live => (Msg::Live.en(), Tone::Pending),
        StreamRealizedState::Starting => (Msg::StreamStarting.en(), Tone::Pending),
        StreamRealizedState::WaitingToReconnect => (Msg::StreamReconnecting.en(), Tone::Pending),
        StreamRealizedState::Congested => (Msg::StreamCongested.en(), Tone::Pending),
        StreamRealizedState::Failed => (Msg::ConnFailed.en(), Tone::Fault),
        StreamRealizedState::Stopped if desired => (Msg::StreamNotLive.en(), Tone::Pending),
        StreamRealizedState::Stopped => (Msg::StreamOff.en(), Tone::Idle),
        StreamRealizedState::Unavailable if desired => (Msg::StreamUnavailable.en(), Tone::Pending),
        StreamRealizedState::Unavailable => (Msg::StreamUnavailable.en(), Tone::Idle),
    };
    let tone = if pending && tone != Tone::Fault {
        Tone::Pending
    } else {
        tone
    };
    let mut detail = match sample {
        Some(sample) => {
            let mut text = l.format(
                Msg::RowStreamCounters,
                &[
                    ("bytes", Arg::Str(&l.bytes(sample.muxed_bytes))),
                    ("count", Arg::Count(sample.dropped_pairs)),
                ],
            );
            if let Some(local) = sample.local {
                let _ = write!(
                    text,
                    " · {}",
                    l.format(Msg::RowSegments, &[("count", Arg::from(local.segments))])
                );
            }
            if let Some(failure) = &sample.failure {
                let _ = write!(text, " · {failure}");
            }
            text
        }
        None if desired => l.text(Msg::RowRequestedNoSample).to_owned(),
        None => l.text(Msg::RowDesiredOff).to_owned(),
    };
    if pending {
        detail = format!("{} · {detail}", l.text(Msg::RowCommandPending));
    }
    OutputRow {
        status,
        tone,
        detail,
    }
}

/// Formats a byte counter with binary units and one decimal in the
/// English console format; see [`fm_ui_model::i18n::Locale::bytes`].
#[cfg(test)]
pub(crate) fn format_bytes(bytes: u64) -> String {
    crate::l10n::Locale::ENGLISH.bytes(bytes)
}

/// Section header summary: how many rows are on air out of how many, such
/// as `2 SLOTS` or `1 / 2 RECORDING`.
pub(crate) fn on_air_summary(on_air: usize, total: usize, noun: &str, active: &str) -> String {
    if on_air == 0 {
        format!("{total} {noun}")
    } else {
        format!("{on_air} / {total} {active}")
    }
}

/// The single key offered on a row.
struct RowKey<'a> {
    starts: bool,
    enabled: bool,
    action: &'a str,
    reason: &'a str,
}

/// Paints one fixed-height row and returns whether its key was clicked.
fn draw_row(
    ui: &mut Ui,
    id: egui::Id,
    title: &str,
    row: &OutputRow,
    tooltip: &str,
    key: &RowKey<'_>,
) -> bool {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), OUTPUT_ROW_HEIGHT),
        Sense::hover(),
    );
    let key_rect = Rect::from_center_size(
        pos2(rect.max.x - 8.0 - KEY_WIDTH / 2.0, rect.center().y),
        Vec2::new(KEY_WIDTH, 26.0),
    );
    let text_left = rect.min.x + 26.0;
    let text_right = key_rect.min.x - 8.0;
    let chip_rect = Rect::from_min_size(
        pos2(text_left, rect.min.y + 22.0),
        Vec2::new(CHIP_WIDTH.min(text_right - text_left), 15.0),
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(4), GRAPHITE);
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(
                1.0,
                if row.tone == Tone::OnAir {
                    PROGRAM.gamma_multiply(0.55)
                } else {
                    BORDER
                },
            ),
            egui::StrokeKind::Inside,
        );
        paint_lamp(
            painter,
            pos2(rect.min.x + 14.0, rect.min.y + 13.0),
            4.0,
            row.tone,
        );
        let title_galley = elided(
            painter,
            title,
            FontId::new(12.5, FontFamily::Proportional),
            TEXT,
            text_right - text_left,
        );
        painter.galley(pos2(text_left, rect.min.y + 5.0), title_galley, TEXT);
        let detail_color = match row.tone {
            Tone::Fault => row.tone.color(),
            Tone::Pending => TEXT_DIM,
            _ => MUTED,
        };
        let detail_left = chip_rect.max.x + 7.0;
        let detail_galley = elided(
            painter,
            &row.detail,
            FontId::new(10.0, FontFamily::Proportional),
            detail_color,
            text_right - detail_left,
        );
        painter.galley(
            pos2(
                detail_left,
                chip_rect.center().y - detail_galley.size().y / 2.0,
            ),
            detail_galley,
            detail_color,
        );
        status_chip_painted(painter, chip_rect, row.status, row.tone);
    }
    ui.interact(
        Rect::from_min_max(rect.min, pos2(key_rect.min.x - 4.0, rect.max.y)),
        id.with("info"),
        Sense::hover(),
    )
    .on_hover_text(tooltip);
    ui.add_space(1.0);
    let (label, fill, stroke) = if key.starts {
        ("START", PROGRAM_FILL, PROGRAM.gamma_multiply(0.6))
    } else {
        ("STOP", WIDGET, BORDER_STRONG)
    };
    ui.place(key_rect, |ui: &mut Ui| {
        ui.add_enabled(
            key.enabled,
            Button::new(RichText::new(label).size(11.0).strong().color(TEXT))
                .fill(fill)
                .stroke(Stroke::new(1.0, stroke))
                .min_size(key_rect.size()),
        )
    })
    .hint(key.action, key.reason)
    .clicked()
}

/// Draws the recorder rows; returns the gated action when a key is clicked.
pub(crate) fn draw_recorders(ui: &mut Ui, state: &StudioUiState) -> Option<RecordingAction> {
    let has_view = state.view.is_some();
    let recording = state
        .recording_desired
        .iter()
        .filter(|desired| has_view && **desired)
        .count();
    section_header(ui, "Recording", |ui| {
        ui.label(
            mono(on_air_summary(
                recording,
                state.recording_desired.len(),
                "SLOTS",
                "RECORDING",
            ))
            .size(10.0)
            .color(if recording > 0 { PROGRAM } else { TEXT_DIM }),
        );
    });
    let mut selected = None;
    for (index, desired) in state.recording_desired.iter().copied().enumerate() {
        let Some(recorder) = u8::try_from(index + 1)
            .ok()
            .and_then(fm_protocol::WireRecorderSlot::new)
        else {
            continue;
        };
        let slot = index + 1;
        let pending = state.recording_pending[index];
        let requested = if desired {
            RecordingAction::Stop(recorder)
        } else {
            RecordingAction::Start(recorder)
        };
        let row = recorder_row(crate::l10n::locale(ui.ctx()), has_view, desired, pending);
        let allowed = recording_action(state, requested);
        let action = if requested.starts() {
            format!("Start program recorder {slot}")
        } else {
            format!("Stop program recorder {slot}")
        };
        let tooltip = format!(
            "Recorder slot {slot}: {}\nShows the daemon's desired recording state; the protocol reports no realized file or duration.",
            row.status
        );
        if draw_row(
            ui,
            egui::Id::new(("studio-recorder-row", slot)),
            &format!("REC {slot}"),
            &row,
            &tooltip,
            &RowKey {
                starts: requested.starts(),
                enabled: allowed.is_some(),
                action: &action,
                reason: if has_view && pending {
                    "an earlier recording command is still pending"
                } else {
                    unavailable_reason(state, Capability::Transition)
                },
            },
        ) {
            selected = allowed;
        }
    }
    selected
}

/// Draws the stream target rows and pushes clicked Start/Stop intents.
pub(crate) fn draw_streams(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    let samples: Vec<_> = state
        .streams
        .iter()
        .map(|stream| latest_stream_sample(state.stream_status.as_ref(), stream.target))
        .collect();
    let rows: Vec<_> = state
        .streams
        .iter()
        .zip(&samples)
        .map(|(stream, sample)| {
            stream_row(
                crate::l10n::locale(ui.ctx()),
                stream,
                *sample,
                stream_command_pending(state, WireStreamTargetId::new(stream.target)),
            )
        })
        .collect();
    let live = rows.iter().filter(|row| row.tone == Tone::OnAir).count();
    section_header(ui, "Streams", |ui| {
        ui.label(
            mono(if state.view.is_some() {
                on_air_summary(live, rows.len(), "TARGETS", "LIVE")
            } else {
                "— TARGETS".to_owned()
            })
            .size(10.0)
            .color(if live > 0 { PROGRAM } else { TEXT_DIM }),
        );
    });
    if state.streams.is_empty() {
        let text = if crate::panels::streams_show_none_configured(
            state.connection_status,
            state.view.is_some(),
            &state.streams,
        ) {
            STREAMS_NONE_CONFIGURED
        } else {
            "Stream state not synchronized"
        };
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), OUTPUT_ROW_HEIGHT),
            Sense::hover(),
        );
        let painter = ui.painter();
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, BORDER),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            FontId::new(11.0, FontFamily::Proportional),
            MUTED,
        );
        return;
    }
    for ((stream, sample), row) in state.streams.iter().zip(samples).zip(&rows) {
        let requested = StreamAction::for_stream(stream);
        let allowed = stream_action(state, requested);
        let action = stream_action_text(requested, &stream.name);
        if draw_row(
            ui,
            egui::Id::new(("studio-stream-row", stream.target)),
            &stream.name,
            row,
            &format!(
                "{}\nDesired {} · realized {}\n{}",
                stream.name,
                if stream.desired_running { "on" } else { "off" },
                row.status.to_lowercase(),
                stream_row_text(stream, sample)
            ),
            &RowKey {
                starts: requested.starts(),
                enabled: allowed.is_some(),
                action: &action,
                reason: stream_unavailable_reason(state, stream),
            },
        ) && let Some(action) = allowed
        {
            intents.push(StudioIntent::Stream(action));
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_protocol::StreamLocalStatus;

    use super::*;

    fn stream(desired_running: bool, realized: StreamRealizedState) -> StreamStatus {
        StreamStatus {
            target: NonZeroU128::new(7).unwrap(),
            name: "Main RTMP".to_owned(),
            desired_running,
            realized,
            detail: None,
        }
    }

    fn sample(realized: StreamRealizedState) -> StreamStatusSample {
        StreamStatusSample {
            target: WireStreamTargetId::new(NonZeroU128::new(7).unwrap()),
            realized,
            connected: true,
            muxed_bytes: 3 * 1_024 * 1_024 / 2,
            enqueued_pairs: 40,
            dropped_pairs: 2,
            failure: None,
            local: None,
        }
    }

    #[test]
    fn recorder_rows_never_claim_state_without_a_view() {
        let unknown = recorder_row(Locale::ENGLISH, false, true, false);
        assert_eq!(unknown.status, "NO STATE");
        assert_eq!(unknown.tone, Tone::Idle);
        assert_eq!(
            recorder_row(Locale::ENGLISH, true, false, false).status,
            "STOPPED"
        );
        assert_eq!(
            recorder_row(Locale::ENGLISH, true, false, false).tone,
            Tone::Idle
        );
        let recording = recorder_row(Locale::ENGLISH, true, true, false);
        assert_eq!(
            (recording.status, recording.tone),
            ("RECORDING", Tone::OnAir)
        );
        let pending = recorder_row(Locale::ENGLISH, true, true, true);
        assert_eq!((pending.status, pending.tone), ("RECORDING", Tone::Pending));
        assert!(pending.detail.starts_with("Command pending"));
    }

    #[test]
    fn stream_rows_are_red_only_when_reported_live() {
        let live = stream_row(
            Locale::ENGLISH,
            &stream(true, StreamRealizedState::Stopped),
            Some(&sample(StreamRealizedState::Live)),
            false,
        );
        assert_eq!((live.status, live.tone), ("LIVE", Tone::OnAir));
        assert_eq!(live.detail, "1.5 MB sent · 2 dropped");
        // Desired on, daemon reports stopped: amber, never red.
        let requested = stream_row(
            Locale::ENGLISH,
            &stream(true, StreamRealizedState::Stopped),
            None,
            false,
        );
        assert_eq!(
            (requested.status, requested.tone),
            ("NOT LIVE", Tone::Pending)
        );
        assert_eq!(requested.detail, "Requested · no transport sample");
        let off = stream_row(
            Locale::ENGLISH,
            &stream(false, StreamRealizedState::Stopped),
            None,
            false,
        );
        assert_eq!(
            (off.status, off.tone, off.detail.as_str()),
            ("OFF", Tone::Idle, "Desired off")
        );
        // Live but desired off: a stop the daemon has not realized yet.
        let stopping = stream_row(
            Locale::ENGLISH,
            &stream(false, StreamRealizedState::Live),
            None,
            false,
        );
        assert_eq!((stopping.status, stopping.tone), ("LIVE", Tone::Pending));
        let failed = stream_row(
            Locale::ENGLISH,
            &stream(true, StreamRealizedState::Failed),
            None,
            true,
        );
        assert_eq!((failed.status, failed.tone), ("FAILED", Tone::Fault));
        assert!(failed.detail.starts_with("Command pending · "));
        let pending = stream_row(
            Locale::ENGLISH,
            &stream(false, StreamRealizedState::Stopped),
            None,
            true,
        );
        assert_eq!(pending.tone, Tone::Pending);
    }

    #[test]
    fn stream_detail_names_local_segments_and_failures() {
        let mut local = sample(StreamRealizedState::Congested);
        local.local = Some(StreamLocalStatus {
            segments: 6,
            target_duration_ms: 2_000,
            free_bytes: 1,
        });
        local.failure = Some("io".to_owned());
        let row = stream_row(
            Locale::ENGLISH,
            &stream(true, StreamRealizedState::Live),
            Some(&local),
            false,
        );
        assert_eq!((row.status, row.tone), ("CONGESTED", Tone::Pending));
        assert_eq!(row.detail, "1.5 MB sent · 2 dropped · 6 seg · io");
    }

    #[test]
    fn byte_counts_and_summaries_are_compact() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1_023), "1023 B");
        assert_eq!(format_bytes(2_048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1_024 * 1_024 * 1_024), "5.0 GB");
        assert_eq!(on_air_summary(0, 2, "SLOTS", "RECORDING"), "2 SLOTS");
        assert_eq!(on_air_summary(1, 2, "TARGETS", "LIVE"), "1 / 2 LIVE");
    }

    #[test]
    fn byte_counts_carry_when_rounding_reaches_the_next_unit() {
        // 1023.999 KB used to print as `1024.0 KB`.
        assert_eq!(format_bytes(1_048_575), "1.0 MB");
        assert_eq!(format_bytes(1_048_520), "1023.9 KB");
        assert_eq!(format_bytes(u64::MAX), "16777216.0 TB");
    }
}
