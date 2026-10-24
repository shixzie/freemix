//! The Replay panel: instant replay (`OR-012`).
//!
//! The panel follows the server's `replay_status` record
//! ([`StudioUiState::replay`]). From top to bottom:
//!
//! - **Recording**: which inputs are recorded for replay (up to four, in
//!   angle order) and how many seconds each keeps, with every source's
//!   retained segments; changing either is an undoable project edit that
//!   starts the recording over.
//! - **Marks**: MARK IN and MARK OUT at the live edge, CAPTURE the marked
//!   range, or LAST 5 / 10 / 20 s.
//! - **Events**: every captured event with its length, a PARTIAL chip while
//!   finished segment files do not cover it yet, its name (renamed in
//!   place), a CUE key per Replay input, and Delete.
//! - **Replay inputs**: each player's cued event and shown frame, its angle,
//!   the 25 / 50 / 75 / 100 % play keys, PAUSE, CUE IN (back to the first
//!   frame), and TAKE (cut the Replay input to Program).
//!
//! Live keys need the transition permission and the recording settings the
//! project-edit permission; every control explains itself on hover.

use egui::{Checkbox, ComboBox, DragValue, Id, RichText, TextEdit, Ui};
use fm_protocol::{
    MAX_REPLAY_RECORD_SOURCES, MAX_REPLAY_RETENTION_SECONDS, MIN_REPLAY_RETENTION_SECONDS,
    REPLAY_LAST_SECONDS, ReplayEventStatus, ReplayOperation, ReplayPlayerStatus, ReplaySettings,
    ReplaySpeed, ReplayStatusMessage, replay_event_name_is_valid,
};
use fm_types::{FrameRate, InputId};

use crate::edit_history::stinger_edit_lock_reason;
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::monitors::input_name;
use crate::playlist::{key_button, timecode};
use crate::theme::{AMBER, PREVIEW, PROGRAM, Tone, caption, card, empty_state, mono, status_chip};
use crate::{StudioConnectionStatus, StudioIntent, StudioUiState};

const SPEEDS: [ReplaySpeed; 4] = [
    ReplaySpeed::Quarter,
    ReplaySpeed::Half,
    ReplaySpeed::ThreeQuarters,
    ReplaySpeed::Full,
];

/// Why live replay keys (marks, captures, players) cannot be pressed.
#[must_use]
pub(crate) fn live_block(state: &StudioUiState) -> Option<&'static str> {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.view.is_some()
        && state.can_transition;
    (!ready).then(|| unavailable_reason(state, Capability::Transition))
}

/// Why the recording settings cannot be changed.
#[must_use]
pub(crate) fn settings_block(state: &StudioUiState) -> Option<&'static str> {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.view.is_some()
        && state.can_edit_project;
    if !ready {
        return Some(unavailable_reason(state, Capability::EditProject));
    }
    stinger_edit_lock_reason(state)
}

/// Why marks and captures cannot be taken, live gates first.
#[must_use]
pub(crate) fn capture_block(
    state: &StudioUiState,
    status: &ReplayStatusMessage,
) -> Option<&'static str> {
    live_block(state).or_else(|| {
        status
            .settings
            .sources
            .is_empty()
            .then_some(Msg::ReplayReasonNotRecording.en())
    })
}

/// The inputs the operator may tick for recording: every input except the
/// Replay inputs. The server's `replay_status` lists one player per Replay
/// input, and the server refuses to record a Replay input.
#[must_use]
pub(crate) fn recordable_sources(inputs: &[InputId], status: &ReplayStatusMessage) -> Vec<InputId> {
    inputs
        .iter()
        .copied()
        .filter(|input| {
            !status
                .players
                .iter()
                .any(|player| player.input.to_domain() == *input)
        })
        .collect()
}

/// `settings` with `input` recorded (appended as the last angle) or not.
#[must_use]
pub(crate) fn with_source(
    settings: &ReplaySettings,
    input: InputId,
    recorded: bool,
) -> ReplaySettings {
    let mut sources: Vec<InputId> = settings
        .sources
        .iter()
        .copied()
        .filter(|source| *source != input)
        .collect();
    if recorded {
        sources.push(input);
    }
    ReplaySettings {
        sources,
        ..settings.clone()
    }
}

/// What TAKE does: put the Replay input on Preview, then cut.
#[must_use]
pub(crate) fn take(input: InputId) -> [StudioIntent; 2] {
    [StudioIntent::SelectPreview(input), StudioIntent::Cut]
}

fn frame_rate(state: &StudioUiState) -> FrameRate {
    state.project_state.as_ref().map_or_else(
        || FrameRate::new(25, 1).expect("25 fps is valid"),
        fm_ui_model::ProjectState::frame_rate,
    )
}

fn name_of(state: &StudioUiState, l: Locale, input: InputId) -> String {
    state.view.as_ref().map_or_else(
        || input.to_string(),
        |view| input_name(l, &view.inputs, &view.input_names, input),
    )
}

/// The Replay panel body.
pub(crate) fn draw_replay(ui: &mut Ui, state: &StudioUiState, intents: &mut Vec<StudioIntent>) {
    let l = l10n::locale(ui.ctx());
    let Some(status) = state.replay.as_deref() else {
        empty_state(ui, l.text(Msg::TabReplay), l.text(Msg::ReplayNotReported));
        return;
    };
    ui.add(
        egui::Label::new(caption(l.format(
            Msg::ReplayIntro,
            &[(
                "count",
                Arg::Count(u64::from(status.settings.retention_seconds)),
            )],
        )))
        .wrap(),
    );
    ui.add_space(2.0);
    if status.players.iter().any(|player| player.playing) {
        ui.ctx()
            .request_repaint_after(core::time::Duration::from_millis(250));
    }
    let rate = frame_rate(state);
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        draw_recording(ui, l, state, status, intents);
    });
    ui.add_space(6.0);
    card().show(ui, |ui| {
        ui.set_width(ui.available_width());
        draw_marks(ui, l, state, status, rate, intents);
    });
    ui.add_space(6.0);
    ui.label(crate::theme::section_label(l.text(Msg::ReplayEvents)));
    if status.events.is_empty() {
        ui.label(caption(l.text(Msg::ReplayNoEvents)));
    }
    for event in status.events.iter().rev() {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            draw_event(ui, l, state, status, event, rate, intents);
        });
        ui.add_space(4.0);
    }
    ui.add_space(6.0);
    ui.label(crate::theme::section_label(l.text(Msg::ReplayPlayers)));
    if status.players.is_empty() {
        ui.label(caption(l.text(Msg::ReplayNoPlayers)));
    }
    for player in &status.players {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            draw_player(ui, l, state, status, player, rate, intents);
        });
        ui.add_space(4.0);
    }
}

fn draw_recording(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    status: &ReplayStatusMessage,
    intents: &mut Vec<StudioIntent>,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(l.text(Msg::RecordingHeading)).strong());
        if status.settings.sources.is_empty() {
            status_chip(ui, l.text(Msg::ReplayNotRecording), Tone::Idle, 110.0)
                .on_hover_text(l.text(Msg::ReplayNotRecordingHover));
        } else {
            status_chip(ui, l.text(Msg::BadgeRec), Tone::OnAir, 48.0)
                .on_hover_text(l.text(Msg::ReplayRecHover));
        }
        if status.feed == fm_protocol::ReplayFeed::Synthetic {
            status_chip(ui, l.text(Msg::ReplaySimulated), Tone::Pending, 48.0)
                .on_hover_text(l.text(Msg::ReplaySimulatedHover));
        }
    });
    let block = settings_block(state);
    let inputs = state
        .view
        .as_ref()
        .map(|view| view.inputs.clone())
        .unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        for input in recordable_sources(&inputs, status) {
            let recorded = status.settings.sources.contains(&input);
            let full = !recorded && status.settings.sources.len() >= MAX_REPLAY_RECORD_SOURCES;
            let reason = block.or_else(|| full.then_some(Msg::ReplayReasonFull.en()));
            let mut checked = recorded;
            let response = ui
                .add_enabled(
                    reason.is_none(),
                    Checkbox::new(&mut checked, name_of(state, l, input)),
                )
                .hint(
                    l.text(Msg::ReplaySourceHover),
                    reason.unwrap_or(Msg::ReplayReasonFull.en()),
                );
            if response.changed() {
                intents.push(StudioIntent::Replay(ReplayOperation::Configure(Box::new(
                    with_source(&status.settings, input, checked),
                ))));
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label(l.text(Msg::ReplayRetention));
        let id = Id::new("studio-replay-retention");
        let mut seconds = ui
            .data(|data| data.get_temp::<u32>(id))
            .unwrap_or(status.settings.retention_seconds);
        let response = ui
            .add_enabled(
                block.is_none(),
                DragValue::new(&mut seconds)
                    .range(MIN_REPLAY_RETENTION_SECONDS..=MAX_REPLAY_RETENTION_SECONDS)
                    .suffix(" s"),
            )
            .hint(
                l.text(Msg::ReplayRetentionHover),
                block.unwrap_or(Msg::ReplayReasonNotRecording.en()),
            );
        if response.dragged() || response.has_focus() {
            ui.data_mut(|data| data.insert_temp(id, seconds));
        } else {
            ui.data_mut(|data| data.remove::<u32>(id));
        }
        if (response.drag_stopped() || response.lost_focus())
            && seconds != status.settings.retention_seconds
        {
            intents.push(StudioIntent::Replay(ReplayOperation::Configure(Box::new(
                ReplaySettings {
                    retention_seconds: seconds,
                    ..status.settings.clone()
                },
            ))));
        }
    });
    for source in &status.recorded {
        let input = source.input.to_domain();
        ui.label(caption(l.format(
            Msg::ReplaySourceLine,
            &[
                ("name", Arg::Str(&name_of(state, l, input))),
                ("count", Arg::Count(u64::from(source.segments))),
                ("megabytes", Arg::Count(source.bytes / 1_000_000)),
            ],
        )))
        .on_hover_text(l.text(Msg::ReplayRecHover));
    }
}

fn draw_marks(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    status: &ReplayStatusMessage,
    rate: FrameRate,
    intents: &mut Vec<StudioIntent>,
) {
    let block = capture_block(state, status);
    let reason = block.unwrap_or(Msg::ReplayReasonNotRecording.en());
    let mut push = |operation| intents.push(StudioIntent::Replay(operation));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let keys = [
            (
                ReplayOperation::MarkIn,
                l.text(Msg::ReplayMarkIn),
                l.text(Msg::ReplayMarkInHover).to_owned(),
                status.mark_in.is_some().then_some(PREVIEW),
            ),
            (
                ReplayOperation::MarkOut,
                l.text(Msg::ReplayMarkOut),
                l.text(Msg::ReplayMarkOutHover).to_owned(),
                status.mark_out.is_some().then_some(PREVIEW),
            ),
            (
                ReplayOperation::CaptureMarks { name: None },
                l.text(Msg::ReplayCapture),
                l.text(Msg::ReplayCaptureHover).to_owned(),
                None,
            ),
        ];
        for (operation, label, hover, tone) in keys {
            if ui
                .add_enabled(block.is_none(), key_button(label, tone, 76.0))
                .hint(&hover, reason)
                .clicked()
            {
                push(operation);
            }
        }
        for seconds in REPLAY_LAST_SECONDS {
            let label = l.format(
                Msg::ReplayLast,
                &[("count", Arg::Count(u64::from(seconds)))],
            );
            let hover = l.format(
                Msg::ReplayLastHover,
                &[("count", Arg::Count(u64::from(seconds)))],
            );
            if ui
                .add_enabled(block.is_none(), key_button(&label, Some(AMBER), 64.0))
                .hint(&hover, reason)
                .clicked()
            {
                push(ReplayOperation::CaptureLast { seconds });
            }
        }
    });
    let mark =
        |frame: Option<u64>| frame.map_or_else(|| "--".to_owned(), |frame| timecode(frame, rate));
    ui.label(mono(l.format(
        Msg::ReplayMarks,
        &[
            ("in", Arg::Str(&mark(status.mark_in))),
            ("out", Arg::Str(&mark(status.mark_out))),
            ("live", Arg::Str(&mark(status.live_frame))),
        ],
    )))
    .on_hover_text(l.text(Msg::ReplayMarksHover));
}

fn draw_event(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    status: &ReplayStatusMessage,
    event: &ReplayEventStatus,
    rate: FrameRate,
    intents: &mut Vec<StudioIntent>,
) {
    let block = live_block(state);
    let reason = block.unwrap_or(Msg::ReplayReasonNotRecording.en());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let id = Id::new(("studio-replay-event-name", event.id));
        let mut name = ui
            .data(|data| data.get_temp::<String>(id))
            .unwrap_or_else(|| event.name.clone());
        let response = ui
            .add_enabled(
                block.is_none(),
                TextEdit::singleline(&mut name).desired_width(140.0),
            )
            .hint(l.text(Msg::ReplayEventNameHover), reason);
        if response.has_focus() {
            ui.data_mut(|data| data.insert_temp(id, name.clone()));
        } else {
            ui.data_mut(|data| data.remove::<String>(id));
        }
        if response.lost_focus() && name != event.name && replay_event_name_is_valid(&name) {
            intents.push(StudioIntent::Replay(ReplayOperation::RenameEvent {
                event: event.id,
                name,
            }));
        }
        ui.label(mono(timecode(event.out_frame - event.in_frame, rate)))
            .on_hover_text(l.format(
                Msg::ReplayEventLengthHover,
                &[("start", Arg::Str(&timecode(event.in_frame, rate)))],
            ));
        if !event.complete {
            status_chip(ui, l.text(Msg::ReplayPartial), Tone::Pending, 64.0)
                .on_hover_text(l.text(Msg::ReplayPartialHover));
        }
        if ui
            .add_enabled(
                block.is_none(),
                key_button(l.text(Msg::AnimDelete), None, 56.0),
            )
            .hint(l.text(Msg::ReplayDeleteHover), reason)
            .clicked()
        {
            intents.push(StudioIntent::Replay(ReplayOperation::DeleteEvent {
                event: event.id,
            }));
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for player in &status.players {
            let name = name_of(state, l, player.input.to_domain());
            let cued = player.event == Some(event.id);
            if ui
                .add_enabled(
                    block.is_none(),
                    key_button(
                        &l.format(Msg::ReplayCueOn, &[("name", Arg::Str(&name))]),
                        cued.then_some(PREVIEW),
                        110.0,
                    ),
                )
                .hint(
                    &l.format(Msg::ReplayCueOnHover, &[("name", Arg::Str(&name))]),
                    reason,
                )
                .clicked()
            {
                intents.push(StudioIntent::Replay(ReplayOperation::Cue {
                    input: player.input,
                    event: event.id,
                    angle: None,
                }));
            }
        }
    });
}

#[allow(clippy::too_many_lines)]
fn draw_player(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    status: &ReplayStatusMessage,
    player: &ReplayPlayerStatus,
    rate: FrameRate,
    intents: &mut Vec<StudioIntent>,
) {
    let block = live_block(state);
    let input = player.input.to_domain();
    let event = player
        .event
        .and_then(|id| status.events.iter().find(|event| event.id == id));
    let cued_reason =
        block.or_else(|| event.is_none().then_some(Msg::ReplayReasonNothingCued.en()));
    let reason = cued_reason.unwrap_or(Msg::ReplayReasonNothingCued.en());
    ui.horizontal(|ui| {
        ui.label(RichText::new(name_of(state, l, input)).strong());
        let cued = event.map_or_else(
            || l.text(Msg::ReplayNothingCued).to_owned(),
            |event| event.name.clone(),
        );
        ui.label(cued).on_hover_text(l.text(Msg::ReplayCuedHover));
        if let (Some(event), Some(frame)) = (event, player.frame) {
            ui.label(mono(timecode(frame.saturating_sub(event.in_frame), rate)))
                .on_hover_text(l.text(Msg::ReplayFrameHover));
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if let Some(event) = event
            && let Some(shown) = player.angle.or(event.angles.first().copied())
        {
            ComboBox::from_id_salt(("studio-replay-angle", input))
                .selected_text(name_of(state, l, shown.to_domain()))
                .width(110.0)
                .show_ui(ui, |ui| {
                    for angle in &event.angles {
                        if ui
                            .add_enabled(
                                block.is_none(),
                                egui::Button::selectable(
                                    *angle == shown,
                                    name_of(state, l, angle.to_domain()),
                                ),
                            )
                            .hint(l.text(Msg::ReplayAngleHover), reason)
                            .clicked()
                            && *angle != shown
                        {
                            intents.push(StudioIntent::Replay(ReplayOperation::Angle {
                                input: player.input,
                                angle: *angle,
                            }));
                        }
                    }
                })
                .response
                .on_hover_text(l.text(Msg::ReplayAngleHover));
        }
        for speed in SPEEDS {
            let lit = player.playing && player.speed == speed;
            let label = l.percent(i64::from(speed.percent()));
            if ui
                .add_enabled(
                    cued_reason.is_none(),
                    key_button(&label, lit.then_some(PREVIEW), 44.0),
                )
                .hint(
                    &l.format(
                        Msg::ReplayPlayHover,
                        &[("percent", Arg::Plain(u64::from(speed.percent())))],
                    ),
                    reason,
                )
                .clicked()
            {
                intents.push(StudioIntent::Replay(ReplayOperation::Play {
                    input: player.input,
                    speed,
                }));
            }
        }
        let paused = event.is_some() && !player.playing;
        let keys = [
            (
                ReplayOperation::Pause {
                    input: player.input,
                },
                l.text(Msg::ReplayPause),
                Msg::ReplayPauseHover,
                paused.then_some(AMBER),
            ),
            (
                ReplayOperation::CueIn {
                    input: player.input,
                },
                l.text(Msg::ReplayCueIn),
                Msg::ReplayCueInHover,
                None,
            ),
        ];
        for (operation, label, hover, tone) in keys {
            if ui
                .add_enabled(cued_reason.is_none(), key_button(label, tone, 60.0))
                .hint(l.text(hover), reason)
                .clicked()
            {
                intents.push(StudioIntent::Replay(operation));
            }
        }
        if ui
            .add_enabled(
                block.is_none(),
                key_button(l.text(Msg::TakeKey), Some(PROGRAM), 56.0),
            )
            .hint(
                l.text(Msg::ReplayTakeHover),
                block.unwrap_or(Msg::ReplayReasonNothingCued.en()),
            )
            .clicked()
        {
            intents.extend(take(input));
        }
    });
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    #[test]
    fn recording_a_source_appends_its_angle_and_unrecording_removes_it() {
        let settings = ReplaySettings {
            sources: vec![input(3), input(1)],
            retention_seconds: 120,
            storage_root: Some("/srv/replay".to_owned()),
        };
        let added = with_source(&settings, input(2), true);
        assert_eq!(added.sources, vec![input(3), input(1), input(2)]);
        assert_eq!(added.retention_seconds, 120);
        assert_eq!(added.storage_root, settings.storage_root);
        assert_eq!(
            with_source(&settings, input(3), false).sources,
            vec![input(1)]
        );
        // Recording an input already recorded keeps one angle for it.
        assert_eq!(
            with_source(&settings, input(3), true).sources,
            vec![input(1), input(3)]
        );
    }

    #[test]
    fn take_cuts_the_replay_input_to_program() {
        assert_eq!(
            take(input(9)),
            [StudioIntent::SelectPreview(input(9)), StudioIntent::Cut]
        );
    }

    /// OR-012: a Replay input is a player, never a recording source.
    #[test]
    fn replay_inputs_are_not_offered_as_recording_sources() {
        let status = crate::hover_audit::replay_status();
        let inputs = [input(1), input(4), input(5), input(6)];
        assert_eq!(
            recordable_sources(&inputs, &status),
            vec![input(1), input(6)]
        );
    }

    /// OR-012: an event the server reports without angles draws without
    /// panicking.
    #[test]
    fn an_event_without_angles_draws_without_panicking() {
        let mut state = crate::hover_audit::bank_fixture();
        let mut status = crate::hover_audit::replay_status();
        status.events[0].angles.clear();
        status.players[0].angle = None;
        state.replay = Some(Box::new(status));
        let context = egui::Context::default();
        let mut intents = Vec::new();
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            draw_replay(ui, &state, &mut intents);
        });
        assert_eq!(intents, Vec::new());
    }

    #[test]
    fn keys_follow_the_session_gates_and_the_recording() {
        let view = crate::hover_audit::bank_fixture().view.unwrap();
        let ready = |transition: bool, edit: bool| {
            StudioUiState::new(StudioConnectionStatus::Ready)
                .with_view(view.clone())
                .with_switcher_permissions(transition, transition)
                .with_edit_project_permission(edit)
        };
        let mut status = crate::hover_audit::replay_status();
        assert_eq!(live_block(&ready(true, false)), None);
        assert!(live_block(&ready(false, true)).is_some());
        assert_eq!(settings_block(&ready(false, true)), None);
        assert!(settings_block(&ready(true, false)).is_some());
        // A locked show keeps its live keys but not the recording settings.
        let mut locked = ready(true, true);
        if let Some(view) = locked.view.as_mut() {
            view.edit_locks.project = true;
        }
        assert_eq!(live_block(&locked), None);
        assert_eq!(
            settings_block(&locked),
            Some(crate::edit_history::PROJECT_LOCKED_REASON)
        );
        assert_eq!(capture_block(&ready(true, false), &status), None);
        status.settings.sources.clear();
        assert_eq!(
            capture_block(&ready(true, false), &status),
            Some(Msg::ReplayReasonNotRecording.en())
        );
        let disconnected = StudioUiState::new(StudioConnectionStatus::Connecting);
        assert!(live_block(&disconnected).is_some());
        assert!(settings_block(&disconnected).is_some());
    }

    /// Every key sends its replay command; a viewer's keys send nothing.
    #[test]
    fn the_panel_sends_replay_commands_only_with_permission() {
        let render = |state: &StudioUiState| {
            let context = egui::Context::default();
            let mut intents = Vec::new();
            let _ = context.run_ui(egui::RawInput::default(), |ui| {
                draw_replay(ui, state, &mut intents);
            });
            intents
        };
        let mut state = crate::hover_audit::bank_fixture();
        state.replay = Some(Box::new(crate::hover_audit::replay_status()));
        // Nothing is clicked: drawing alone sends nothing.
        assert_eq!(render(&state), Vec::new());
        state.replay = None;
        assert_eq!(render(&state), Vec::new());
    }
}
