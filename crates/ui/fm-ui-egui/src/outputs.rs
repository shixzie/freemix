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
    Button, Color32, CornerRadius, FontFamily, FontId, Rect, RichText, Sense, Stroke, Ui, Vec2,
    pos2,
};
use fm_protocol::{StreamRealizedState, StreamStatusSample, WireStreamTargetId};
use fm_ui_model::StreamStatus;

use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::panels::{STREAMS_NONE_CONFIGURED, latest_stream_sample, stream_row_text};
use crate::stream_control::{stream_action_text, stream_unavailable_reason};
use crate::theme::{
    AMBER, PROGRAM, PROGRAM_FILL, Tone, border_color, border_strong_color, elided, graphite_color,
    mono, muted_color, paint_lamp, section_header, status_chip_painted, text_color, text_dim_color,
    widget_color,
};
use crate::{
    RecordingAction, StreamAction, StudioIntent, StudioUiState, recording_action, stream_action,
    stream_command_pending,
};

/// Height of one output row; rows never change height with their state.
pub(crate) const OUTPUT_ROW_HEIGHT: f32 = 38.0;
/// Height of the keys inside a row.
const ROW_KEY_HEIGHT: f32 = crate::theme::MIN_TARGET;
/// Room kept clear right of a section header's count, so it never runs
/// into the panel's edge or its scroll bar.
const SCROLL_GUTTER: f32 = 10.0;
/// Wide enough for the longest translated row status (`CONGESTIONADO`).
const CHIP_WIDTH: f32 = 86.0;
/// Narrowest Start/Stop key; wider when a translated word needs it.
const KEY_WIDTH: f32 = 58.0;

/// Width of the Start/Stop key column: both words fit, so every row's key
/// has the same width whichever action it offers.
fn key_width(ui: &Ui, l: Locale) -> f32 {
    [Msg::StartKey, Msg::StopKey]
        .into_iter()
        .map(|word| {
            ui.painter()
                .layout_no_wrap(
                    l.text(word).to_owned(),
                    FontId::new(crate::theme::legible(11.0), FontFamily::Proportional),
                    text_color(),
                )
                .size()
                .x
                + 16.0
        })
        .fold(KEY_WIDTH, f32::max)
        .ceil()
}

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

/// What an outputs section header counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputKind {
    /// Recorder slots, on air while recording.
    Recorders,
    /// Stream targets, on air while live.
    Streams,
}

/// Section header summary: how many rows are on air out of how many, such
/// as `2 SLOTS` or `1 / 2 RECORDING`.
pub(crate) fn on_air_summary(l: Locale, on_air: usize, total: usize, kind: OutputKind) -> String {
    if on_air == 0 {
        l.format(
            match kind {
                OutputKind::Recorders => Msg::SlotsCount,
                OutputKind::Streams => Msg::TargetsCount,
            },
            &[("count", Arg::from(total))],
        )
    } else {
        l.format(
            match kind {
                OutputKind::Recorders => Msg::RecordingOfTotal,
                OutputKind::Streams => Msg::LiveOfTotal,
            },
            &[("on_air", Arg::from(on_air)), ("total", Arg::from(total))],
        )
    }
}

/// The single key offered on a row.
struct RowKey<'a> {
    starts: bool,
    enabled: bool,
    action: &'a str,
    reason: &'a str,
}

/// The row key's label, fill, and outline. Red is reserved for on air: an
/// idle START key is a neutral console key, the STOP key of an output that
/// is on air is lit red like the row's lamp and chip, and a STOP key for a
/// requested-but-not-realized output takes the row's amber.
pub(crate) fn row_key_style(starts: bool, tone: Tone) -> (Msg, Color32, Color32) {
    match (starts, tone) {
        (true, _) => (Msg::StartKey, widget_color(), border_strong_color()),
        (false, Tone::OnAir) => (Msg::StopKey, PROGRAM_FILL, PROGRAM.gamma_multiply(0.8)),
        (false, Tone::Pending) => (Msg::StopKey, widget_color(), AMBER.gamma_multiply(0.7)),
        (false, _) => (Msg::StopKey, widget_color(), border_strong_color()),
    }
}

/// Extra keys drawn inside a row, left of its Start/Stop key: their
/// side-by-side width and a drawer told whether to collapse them into one
/// menu key (`true`) and how tall a key is.
type RowExtras<'a> = (f32, &'a mut dyn FnMut(&mut Ui, bool, f32));

/// Width of the row's text column below which its extra keys collapse into
/// one menu key: the status chip and a few words of detail.
const MIN_TEXT_WIDTH: f32 = CHIP_WIDTH + 60.0;

/// Where a row's extra keys go, left of its Start/Stop key at `key_rect`,
/// and whether they collapse into one menu key so the text column from
/// `text_left` keeps at least [`MIN_TEXT_WIDTH`].
fn extras_layout(key_rect: Rect, text_left: f32, inline_width: f32) -> (Rect, bool) {
    let right = key_rect.min.x - 6.0;
    let collapsed = right - text_left - inline_width - 8.0 < MIN_TEXT_WIDTH;
    let width = if collapsed {
        crate::stream_targets::ROW_MENU_WIDTH
    } else {
        inline_width
    };
    (
        Rect::from_min_max(
            pos2(right - width, key_rect.min.y),
            pos2(right, key_rect.max.y),
        ),
        collapsed,
    )
}

/// Paints one fixed-height row, with any `extras` keys inside it, and
/// returns whether its key was clicked.
fn draw_row(
    ui: &mut Ui,
    id: egui::Id,
    (title, row, tooltip): (&str, &OutputRow, &str),
    key: &RowKey<'_>,
    extras: Option<RowExtras<'_>>,
) -> bool {
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), OUTPUT_ROW_HEIGHT),
        Sense::hover(),
    );
    let key_width = key_width(ui, l10n::locale(ui.ctx()));
    let key_rect = Rect::from_center_size(
        pos2(rect.max.x - 7.0 - key_width / 2.0, rect.center().y),
        Vec2::new(key_width, ROW_KEY_HEIGHT),
    );
    let text_left = rect.min.x + 26.0;
    let extras_rect = extras
        .as_ref()
        .map(|(inline_width, _)| extras_layout(key_rect, text_left, *inline_width));
    let text_right = extras_rect.map_or(key_rect.min.x, |(extras, _)| extras.min.x) - 8.0;
    let chip_rect = Rect::from_min_size(
        pos2(text_left, rect.min.y + 20.0),
        Vec2::new(CHIP_WIDTH.min(text_right - text_left), 15.0),
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(4), graphite_color());
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(
                crate::theme::line_width(),
                if row.tone == Tone::OnAir {
                    PROGRAM.gamma_multiply(0.55)
                } else {
                    border_color()
                },
            ),
            egui::StrokeKind::Inside,
        );
        paint_lamp(
            painter,
            pos2(rect.min.x + 14.0, rect.min.y + 11.5),
            4.0,
            row.tone,
        );
        let title_galley = elided(
            painter,
            title,
            FontId::new(12.5, FontFamily::Proportional),
            text_color(),
            text_right - text_left,
        );
        painter.galley(
            pos2(text_left, rect.min.y + 3.5),
            title_galley,
            text_color(),
        );
        let detail_color = match row.tone {
            Tone::Fault => row.tone.color(),
            Tone::Pending => text_dim_color(),
            _ => muted_color(),
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
        Rect::from_min_max(rect.min, pos2(text_right + 4.0, rect.max.y)),
        id.with("info"),
        Sense::hover(),
    )
    .on_hover_text(tooltip);
    ui.add_space(1.0);
    if let (Some((_, draw)), Some((extras, collapsed))) = (extras, extras_rect) {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(id.with("extras"))
                .max_rect(extras)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        draw(&mut child, collapsed, extras.height());
    }
    draw_row_key(ui, key_rect, key, row.tone)
}

/// The row's Start/Stop key at `key_rect`; returns whether it was clicked.
fn draw_row_key(ui: &mut Ui, key_rect: Rect, key: &RowKey<'_>, tone: Tone) -> bool {
    let l = l10n::locale(ui.ctx());
    let (label, fill, stroke) = row_key_style(key.starts, tone);
    let label = l.text(label);
    ui.place(key_rect, |ui: &mut Ui| {
        ui.add_enabled(
            key.enabled,
            Button::new(
                RichText::new(label)
                    .size(crate::theme::legible(11.0))
                    .strong()
                    .color(text_color()),
            )
            .fill(fill)
            .stroke(Stroke::new(crate::theme::line_width(), stroke))
            .min_size(key_rect.size())
            .truncate(),
        )
    })
    .hint(key.action, key.reason)
    .clicked()
}

/// Draws the recorder rows; returns the gated action when a key is clicked.
pub(crate) fn draw_recorders(ui: &mut Ui, state: &StudioUiState) -> Option<RecordingAction> {
    let l = l10n::locale(ui.ctx());
    let has_view = state.view.is_some();
    let recording = state
        .recording_desired
        .iter()
        .filter(|desired| has_view && **desired)
        .count();
    section_header(ui, l.text(Msg::RecordingHeading), |ui| {
        ui.add_space(SCROLL_GUTTER);
        ui.label(
            mono(on_air_summary(
                l,
                recording,
                state.recording_desired.len(),
                OutputKind::Recorders,
            ))
            .size(crate::theme::legible(10.0))
            .color(if recording > 0 {
                PROGRAM
            } else {
                text_dim_color()
            }),
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
        let row = recorder_row(l, has_view, desired, pending);
        let allowed = recording_action(state, requested);
        let slot_arg = [("slot", Arg::Plain(slot as u64))];
        let action = l.format(
            if requested.starts() {
                Msg::StartRecorder
            } else {
                Msg::StopRecorder
            },
            &slot_arg,
        );
        let tooltip = l.format(
            Msg::RecorderRowHover,
            &[slot_arg[0], ("status", Arg::Str(l.tr(row.status)))],
        );
        if draw_row(
            ui,
            egui::Id::new(("studio-recorder-row", slot)),
            (&format!("REC {slot}"), &row, &tooltip),
            &RowKey {
                starts: requested.starts(),
                enabled: allowed.is_some(),
                action: &action,
                reason: if has_view && pending {
                    Msg::ReasonRecordingPending.en()
                } else {
                    unavailable_reason(state, Capability::Transition)
                },
            },
            None,
        ) {
            selected = allowed;
        }
    }
    selected
}

/// `H:MM:SS` for a duration in milliseconds.
pub(crate) fn uptime_text(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    format!(
        "{}:{:02}:{:02}",
        seconds / 3_600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

/// The destination statistics in a stream row's tooltip (OR-006): its
/// URL without the key, the output and target bitrate, whether a key is
/// stored, and from the latest sample the measured bitrate, dropped frames,
/// reconnects, uptime, and last error. Never any key text.
pub(crate) fn stream_stats_text(
    l: Locale,
    state: &StudioUiState,
    stream: &StreamStatus,
    status: &str,
    sample: Option<&StreamStatusSample>,
) -> String {
    let mut lines = vec![l.format(Msg::StreamStatState, &[("state", Arg::Str(status))])];
    let entry = state
        .view
        .as_ref()
        .and_then(|view| view.stream_target(stream.target));
    if let Some(entry) = entry {
        let config = &entry.config;
        lines.push(l.format(
            Msg::StreamStatUrl,
            &[("url", Arg::Str(&config.display_url()))],
        ));
        if let Some(backup) = &config.backup_endpoint {
            lines.push(l.format(
                Msg::StreamStatBackup,
                &[(
                    "url",
                    Arg::Str(&format!("{}://{backup}", config.protocol.token())),
                )],
            ));
        }
        let output = state
            .view
            .as_ref()
            .and_then(|view| {
                view.outputs
                    .iter()
                    .find(|candidate| candidate.output == config.output.to_domain())
            })
            .map_or_else(
                || config.output.get().to_string(),
                |found| found.name.clone(),
            );
        lines.push(l.format(
            Msg::StreamStatOutput,
            &[
                ("output", Arg::Str(&output)),
                ("target", Arg::Count(u64::from(config.video_bitrate_kbps))),
            ],
        ));
        if config.protocol.carries_key() {
            lines.push(
                l.text(if entry.has_key {
                    Msg::StreamStatKeyStored
                } else {
                    Msg::StreamStatKeyMissing
                })
                .to_owned(),
            );
        }
    }
    if let Some(sample) = sample {
        lines.push(l.format(
            Msg::StreamStatLive,
            &[
                ("kbps", Arg::Count(u64::from(sample.bitrate_kbps))),
                ("dropped", Arg::Count(sample.dropped_pairs)),
                ("reconnects", Arg::Count(sample.reconnects)),
                ("uptime", Arg::Str(&uptime_text(sample.uptime_ms))),
            ],
        ));
        // The last error follows in the transport diagnostics.
    }
    lines.join("\n")
}

/// Draws the stream target rows: Start all / Stop all / Add keys, then per
/// row its Start/Stop key and its Retry / Edit / Remove keys.
#[allow(clippy::too_many_lines)] // one panel section
pub(crate) fn draw_streams(
    ui: &mut Ui,
    state: &StudioUiState,
    dialogs: &mut crate::stream_targets::StreamTargetsUi,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
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
                l,
                stream,
                *sample,
                stream_command_pending(state, WireStreamTargetId::new(stream.target)),
            )
        })
        .collect();
    let live = rows.iter().filter(|row| row.tone == Tone::OnAir).count();
    section_header(ui, l.text(Msg::StreamsHeading), |ui| {
        ui.add_space(SCROLL_GUTTER);
        ui.label(
            mono(if state.view.is_some() {
                on_air_summary(l, live, rows.len(), OutputKind::Streams)
            } else {
                l.text(Msg::TargetsUnknown).to_owned()
            })
            .size(crate::theme::legible(10.0))
            .color(if live > 0 { PROGRAM } else { text_dim_color() }),
        );
    });
    crate::stream_targets::draw_group_keys(ui, state, dialogs, intents);
    if state.streams.is_empty() {
        let text = if crate::panels::streams_show_none_configured(
            state.connection_status,
            state.view.is_some(),
            &state.streams,
        ) {
            l.tr(STREAMS_NONE_CONFIGURED)
        } else {
            l.text(Msg::StreamsNotSynced)
        };
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), OUTPUT_ROW_HEIGHT),
            Sense::hover(),
        );
        let painter = ui.painter();
        painter.rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(crate::theme::line_width(), border_color()),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            FontId::new(11.0, FontFamily::Proportional),
            muted_color(),
        );
        return;
    }
    let inline_width = crate::stream_targets::row_keys_width(ui);
    for ((stream, sample), row) in state.streams.iter().zip(samples).zip(&rows) {
        let requested = StreamAction::for_stream(stream);
        let allowed = stream_action(state, requested);
        let action = stream_action_text(l, requested, &stream.name);
        let tooltip = format!(
            "{}\n{}\n{}",
            l.format(
                Msg::StreamRowHover,
                &[
                    ("name", Arg::Str(&stream.name)),
                    (
                        "desired",
                        Arg::Str(l.text(if stream.desired_running {
                            Msg::WordOn
                        } else {
                            Msg::WordOff
                        })),
                    ),
                    ("realized", Arg::Str(&l.tr(row.status).to_lowercase())),
                ],
            ),
            stream_stats_text(l, state, stream, l.tr(row.status), sample),
            stream_row_text(l, sample)
        );
        let target = WireStreamTargetId::new(stream.target);
        let mut row_keys = |ui: &mut Ui, collapsed: bool, height: f32| {
            crate::stream_targets::draw_row_keys(
                ui,
                state,
                (target, &stream.name),
                (collapsed, height),
                dialogs,
                intents,
            );
        };
        let clicked = draw_row(
            ui,
            egui::Id::new(("studio-stream-row", stream.target)),
            (&stream.name, row, &tooltip),
            &RowKey {
                starts: requested.starts(),
                enabled: allowed.is_some(),
                action: &action,
                reason: stream_unavailable_reason(state, stream),
            },
            Some((inline_width, &mut row_keys)),
        );
        if clicked && let Some(action) = allowed {
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
            bitrate_kbps: 0,
            reconnects: 0,
            uptime_ms: 0,
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
    fn row_keys_are_red_only_while_on_air() {
        assert_eq!(
            row_key_style(true, Tone::Idle),
            (Msg::StartKey, widget_color(), border_strong_color())
        );
        // A START key never turns red, whatever the row shows.
        assert_eq!(row_key_style(true, Tone::OnAir).1, widget_color());
        assert_eq!(row_key_style(false, Tone::OnAir).1, PROGRAM_FILL);
        assert_eq!(
            row_key_style(false, Tone::Pending).2,
            crate::theme::AMBER.gamma_multiply(0.7)
        );
        assert_eq!(row_key_style(false, Tone::Fault).1, widget_color());
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
    fn row_keys_sit_inside_the_row_and_fold_in_a_narrow_dock() {
        let key = |right: f32| Rect::from_min_max(pos2(right - 58.0, 7.0), pos2(right, 31.0));
        // A wide row: the keys sit side by side just left of Start/Stop.
        let (rect, collapsed) = extras_layout(key(800.0), 26.0, 130.0);
        assert!(!collapsed);
        assert_eq!((rect.min.x, rect.max.x), (606.0, 736.0));
        assert_eq!((rect.min.y, rect.max.y), (7.0, 31.0));
        // A narrow dock keeps the text column and folds them into `···`.
        let (rect, collapsed) = extras_layout(key(340.0), 26.0, 130.0);
        assert!(collapsed);
        assert_eq!(rect.width(), crate::stream_targets::ROW_MENU_WIDTH);
        assert_eq!(rect.max.x, 276.0);
    }

    #[test]
    fn byte_counts_and_summaries_are_compact() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1_023), "1023 B");
        assert_eq!(format_bytes(2_048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1_024 * 1_024 * 1_024), "5.0 GB");
        let en = Locale::ENGLISH;
        assert_eq!(on_air_summary(en, 0, 2, OutputKind::Recorders), "2 SLOTS");
        assert_eq!(on_air_summary(en, 0, 1, OutputKind::Recorders), "1 SLOT");
        assert_eq!(on_air_summary(en, 1, 2, OutputKind::Streams), "1 / 2 LIVE");
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        assert_eq!(
            on_air_summary(spanish, 1, 2, OutputKind::Recorders),
            "1 / 2 GRABANDO"
        );
        assert_eq!(
            on_air_summary(spanish, 0, 2, OutputKind::Streams),
            "2 DESTINOS"
        );
    }

    #[test]
    fn byte_counts_carry_when_rounding_reaches_the_next_unit() {
        // 1023.999 KB used to print as `1024.0 KB`.
        assert_eq!(format_bytes(1_048_575), "1.0 MB");
        assert_eq!(format_bytes(1_048_520), "1023.9 KB");
        assert_eq!(format_bytes(u64::MAX), "16777216.0 TB");
    }
}
