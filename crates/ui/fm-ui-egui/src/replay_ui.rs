//! The Replay panel: instant replay (`OR-012`).
//!
//! The panel follows the server's `replay_status` record
//! ([`StudioUiState::replay`]) and, for the REELS section, its
//! `reel_status` record ([`StudioUiState::reels`] and
//! [`StudioUiState::reel_exports`]). From top to bottom:
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
//!   frame), and TAKE (cut the Replay input to Program), with the player's
//!   playlist under its transport (Protocol 2.119): up to sixteen events,
//!   each with its angle and speed, that a playing player moves on to in
//!   turn, and ADD CUED, NEXT, PREV, and CLEAR keys.
//! - **Highlight reels** (OR-013): the reels, the selected reel's clips
//!   (angle, trims, speed, order), and the export jobs with their progress,
//!   Cancel, and Forget keys.
//!
//! Live keys need the transition permission and the recording settings the
//! project-edit permission; every control explains itself on hover.

use egui::{Checkbox, ComboBox, DragValue, Id, RichText, TextEdit, Ui};
use fm_protocol::{
    MAX_REEL_CLIPS, MAX_REEL_DISSOLVE_FRAMES, MAX_REEL_EXPORTS, MAX_REEL_MUSIC_PERCENT,
    MAX_REEL_TRIM_FRAMES, MAX_REELS, MAX_REPLAY_AUTO_RETURN_FRAMES, MAX_REPLAY_PLAYLIST_ENTRIES,
    MAX_REPLAY_RECORD_SOURCES, MAX_REPLAY_RETENTION_SECONDS, MIN_REPLAY_RETENTION_SECONDS,
    REPLAY_LAST_SECONDS, ReelClip, ReelExportState, ReelExportStatus, ReelMusic, ReelOperation,
    ReelSpec, ReelStatus, ReplayEventStatus, ReplayOperation, ReplayPlayerStatus,
    ReplayPlaylistEntry, ReplaySettings, ReplaySpeed, ReplayStatusMessage,
    reel_export_path_is_valid, reel_name_is_valid, replay_event_name_is_valid,
};
use fm_types::{FrameRate, InputId};

use crate::console_keymap::ConsoleAction;
use crate::edit_history::stinger_edit_lock_reason;
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::monitors::input_name;
use crate::playlist::{key_button, timecode};
use crate::theme::{
    AMBER, PREVIEW, PROGRAM, Tone, add_icon_button, caption, card, empty_state, mono,
    section_label, small_button, status_chip,
};
use crate::{StudioConnectionStatus, StudioIntent, StudioUiState};

const SPEEDS: [ReplaySpeed; 4] = [
    ReplaySpeed::Quarter,
    ReplaySpeed::Half,
    ReplaySpeed::ThreeQuarters,
    ReplaySpeed::Full,
];

/// The reel clip speeds the server accepts, in percent.
const REEL_SPEEDS: [u8; 4] = [25, 50, 75, 100];

/// Whether export `state` is still running (and can be cancelled).
const fn export_is_active(state: ReelExportState) -> bool {
    matches!(
        state,
        ReelExportState::Queued | ReelExportState::Running | ReelExportState::Finalizing
    )
}

/// The catalog label of one export state.
const fn export_state_msg(state: ReelExportState) -> Msg {
    match state {
        ReelExportState::Queued => Msg::ReelStateQueued,
        ReelExportState::Running => Msg::ReelStateRunning,
        ReelExportState::Finalizing => Msg::ReelStateFinalizing,
        ReelExportState::Completed => Msg::ReelStateCompleted,
        ReelExportState::Failed => Msg::ReelStateFailed,
        ReelExportState::Cancelled => Msg::ReelStateCancelled,
    }
}

/// The lamp of one export state.
const fn export_tone(state: ReelExportState) -> Tone {
    match state {
        ReelExportState::Queued | ReelExportState::Running | ReelExportState::Finalizing => {
            Tone::Pending
        }
        ReelExportState::Completed => Tone::Ready,
        ReelExportState::Failed => Tone::Fault,
        ReelExportState::Cancelled => Tone::Idle,
    }
}

/// The reel a click on a row selected, or the first reel when none is.
fn selected_reel(state: &StudioUiState, selected: Option<u64>) -> Option<&ReelStatus> {
    selected
        .and_then(|id| state.reels.iter().find(|reel| reel.id == id))
        .or_else(|| state.reels.first())
}

/// The event the EVENTS list selected, or the newest event when none is.
fn selected_event(
    status: &ReplayStatusMessage,
    selected: Option<u32>,
) -> Option<&ReplayEventStatus> {
    selected
        .and_then(|id| status.events.iter().find(|event| event.id == id))
        .or_else(|| status.events.last())
}

/// The id of the reel list's selection.
fn selected_reel_id(ui: &Ui) -> Option<u64> {
    ui.data(|data| data.get_temp::<u64>(Id::new("studio-reel-selected")))
}

/// Selects reel `reel` in the list.
fn select_reel(ui: &mut Ui, reel: u64) {
    ui.data_mut(|data| data.insert_temp(Id::new("studio-reel-selected"), reel));
    set_pending_delete(ui, None);
}

/// The id of the EVENTS list's selection.
fn selected_event_id(ui: &Ui) -> Option<u32> {
    ui.data(|data| data.get_temp::<u32>(Id::new("studio-reel-event")))
}

/// Selects event `event` as the one Add event uses.
fn select_event(ui: &mut Ui, event: u32) {
    ui.data_mut(|data| data.insert_temp(Id::new("studio-reel-event"), event));
}

/// The temp-data id of the reel awaiting delete confirmation.
pub(crate) fn reel_delete_id() -> Id {
    Id::new("studio-reel-delete")
}

/// The temp-data id of one reel's export destination draft.
pub(crate) fn reel_destination_id(reel: u64) -> Id {
    Id::new(("studio-reel-destination", reel))
}

fn pending_delete(ui: &Ui) -> Option<u64> {
    ui.data(|data| data.get_temp::<u64>(reel_delete_id()))
}

fn set_pending_delete(ui: &mut Ui, reel: Option<u64>) {
    match reel {
        Some(reel) => {
            ui.data_mut(|data| data.insert_temp(reel_delete_id(), reel));
        }
        None => {
            ui.data_mut(|data| data.remove::<u64>(reel_delete_id()));
        }
    }
}

/// A reel with the lowest free id (1 through [`MAX_REELS`]) and `event` as
/// its first clip, or `None` when every id is taken or the event has no
/// recorded angle. The server refuses an empty reel, so New reel starts
/// from the selected event.
fn new_reel(reels: &[ReelStatus], event: &ReplayEventStatus) -> Option<ReelSpec> {
    if reels.len() >= MAX_REELS {
        return None;
    }
    let max = u64::try_from(MAX_REELS).unwrap_or(u64::MAX);
    let id = (1..=max).find(|id| reels.iter().all(|reel| reel.id != *id))?;
    let angle = event.angles.first()?.to_domain();
    Some(ReelSpec {
        id,
        name: format!("Reel {id}"),
        clips: vec![clip_from(event, angle)],
        music: None,
    })
}

/// The default clip of `event` at `angle`: untrimmed at 100% speed.
const fn clip_from(event: &ReplayEventStatus, angle: InputId) -> ReelClip {
    ReelClip {
        event: event.id,
        angle,
        trim_in_frames: 0,
        trim_out_frames: 0,
        speed_percent: 100,
        dissolve_frames: 0,
    }
}

/// Appends `event` to `reel` at its first angle, or `None` at the clip
/// limit or when the event has no recorded angle.
fn add_event_clip(reel: &ReelStatus, event: &ReplayEventStatus) -> Option<ReelOperation> {
    if reel.spec.clips.len() >= MAX_REEL_CLIPS {
        return None;
    }
    let angle = event.angles.first()?.to_domain();
    let mut spec = reel.spec.clone();
    spec.clips.push(clip_from(event, angle));
    Some(ReelOperation::Set(Box::new(spec)))
}

/// Renames `reel`, or `None` for an unchanged or unusable name.
fn rename_reel(reel: &ReelStatus, name: &str) -> Option<ReelOperation> {
    if name == reel.spec.name || !reel_name_is_valid(name) {
        return None;
    }
    let mut spec = reel.spec.clone();
    name.clone_into(&mut spec.name);
    Some(ReelOperation::Set(Box::new(spec)))
}

/// Replaces clip `index` of `reel`, or `None` when unchanged or absent.
fn set_clip(reel: &ReelStatus, index: usize, clip: ReelClip) -> Option<ReelOperation> {
    let mut spec = reel.spec.clone();
    let current = spec.clips.get_mut(index)?;
    if *current == clip {
        return None;
    }
    *current = clip;
    Some(ReelOperation::Set(Box::new(spec)))
}

/// Removes clip `index` from `reel`; the last clip stays because a reel
/// must hold at least one. A clip that becomes first cuts in.
fn remove_clip(reel: &ReelStatus, index: usize) -> Option<ReelOperation> {
    if reel.spec.clips.len() <= 1 || index >= reel.spec.clips.len() {
        return None;
    }
    let mut spec = reel.spec.clone();
    spec.clips.remove(index);
    spec.clips[0].dissolve_frames = 0;
    Some(ReelOperation::Set(Box::new(spec)))
}

/// Moves clip `index` one place earlier (`up`) or later in `reel`.
fn move_clip(reel: &ReelStatus, index: usize, up: bool) -> Option<ReelOperation> {
    let target = if up {
        index.checked_sub(1)?
    } else {
        index.checked_add(1)?
    };
    if target >= reel.spec.clips.len() {
        return None;
    }
    let mut spec = reel.spec.clone();
    spec.clips.swap(index, target);
    // The first clip always cuts in.
    spec.clips[0].dissolve_frames = 0;
    Some(ReelOperation::Set(Box::new(spec)))
}

/// The level a new music bed starts at, in percent.
const DEFAULT_MUSIC_LEVEL: u8 = 80;
/// The ducked level a new music bed starts at, in percent of its level.
const DEFAULT_MUSIC_DUCK: u8 = 25;

/// Gives `reel` the music bed `music` (or none), or `None` when unchanged
/// or unusable.
fn set_music(reel: &ReelStatus, music: Option<ReelMusic>) -> Option<ReelOperation> {
    if music == reel.spec.music || music.as_ref().is_some_and(|music| !music.is_valid()) {
        return None;
    }
    let mut spec = reel.spec.clone();
    spec.music = music;
    Some(ReelOperation::Set(Box::new(spec)))
}

/// `reel`'s bed playing the file at `path`, keeping its levels (a new bed
/// takes the default ones).
fn music_with_path(reel: &ReelStatus, path: &str) -> ReelMusic {
    ReelMusic {
        path: path.trim().to_owned(),
        ..reel.spec.music.clone().unwrap_or(ReelMusic {
            path: String::new(),
            level_percent: DEFAULT_MUSIC_LEVEL,
            duck_percent: DEFAULT_MUSIC_DUCK,
        })
    }
}

/// The export command of `reel` to `destination` as job `job`.
fn export_reel(reel: &ReelStatus, job: u64, destination: &str) -> Option<ReelOperation> {
    if job == 0
        || reel.error.is_some()
        || reel.spec.clips.is_empty()
        || !reel_export_path_is_valid(destination)
    {
        return None;
    }
    Some(ReelOperation::Export {
        job,
        reel: reel.id,
        destination: destination.to_owned(),
    })
}

/// The lowest unused export job id (1 through [`MAX_REEL_EXPORTS`]).
fn next_export_job(exports: &[ReelExportStatus]) -> Option<u64> {
    (1..=u64::try_from(MAX_REEL_EXPORTS).unwrap_or(u64::MAX))
        .find(|job| exports.iter().all(|export| export.job != *job))
}

/// Why live replay keys (marks, captures, players) cannot be pressed.
#[must_use]
pub(crate) fn live_block(state: &StudioUiState) -> Option<&'static str> {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.view.is_some()
        && state.can_transition;
    (!ready).then(|| unavailable_reason(state, Capability::Transition))
}

/// What a profile's replay mark or capture key does: `operation`, while
/// the marks can be taken.
pub(crate) fn key_intent(
    state: &StudioUiState,
    operation: ReplayOperation,
) -> Option<StudioIntent> {
    let status = state.replay.as_deref()?;
    capture_block(state, status)
        .is_none()
        .then_some(StudioIntent::Replay(operation))
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
    ui.add_space(6.0);
    draw_reels(ui, l, state, status, intents);
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
    draw_auto_return(ui, l, status, block, intents);
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

/// Auto return (Protocol 2.116): whether a Replay input that plays its
/// event to the end on Program hands it back to the live source, and the
/// mix's frames.
fn draw_auto_return(
    ui: &mut Ui,
    l: Locale,
    status: &ReplayStatusMessage,
    block: Option<&'static str>,
    intents: &mut Vec<StudioIntent>,
) {
    let reason = block.unwrap_or(Msg::ReplayReasonNotRecording.en());
    let configure = |auto_return| {
        StudioIntent::Replay(ReplayOperation::Configure(Box::new(ReplaySettings {
            auto_return,
            ..status.settings.clone()
        })))
    };
    ui.horizontal(|ui| {
        let mut on = status.settings.auto_return.is_some();
        if ui
            .add_enabled(
                block.is_none(),
                Checkbox::new(&mut on, l.text(Msg::ReplayAutoReturn)),
            )
            .hint(l.text(Msg::ReplayAutoReturnHover), reason)
            .changed()
        {
            intents.push(configure(on.then_some(0)));
        }
        let Some(current) = status.settings.auto_return else {
            return;
        };
        let id = Id::new("studio-replay-auto-return");
        let mut frames = ui.data(|data| data.get_temp::<u32>(id)).unwrap_or(current);
        let response = ui
            .add_enabled(
                block.is_none(),
                DragValue::new(&mut frames)
                    .range(0..=MAX_REPLAY_AUTO_RETURN_FRAMES)
                    .suffix(" fr"),
            )
            .hint(l.text(Msg::ReplayAutoReturnFramesHover), reason);
        if response.dragged() || response.has_focus() {
            ui.data_mut(|data| data.insert_temp(id, frames));
        } else {
            ui.data_mut(|data| data.remove::<u32>(id));
        }
        if (response.drag_stopped() || response.lost_focus()) && frames != current {
            intents.push(configure(Some(frames)));
        }
    });
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
    let keymap = state.console_keymap();
    let mut push = |operation| intents.push(StudioIntent::Replay(operation));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let keys = [
            (
                ReplayOperation::MarkIn,
                l.text(Msg::ReplayMarkIn),
                keymap.hint(
                    l,
                    l.text(Msg::ReplayMarkInHover),
                    ConsoleAction::ReplayMarkIn,
                ),
                status.mark_in.is_some().then_some(PREVIEW),
            ),
            (
                ReplayOperation::MarkOut,
                l.text(Msg::ReplayMarkOut),
                keymap.hint(
                    l,
                    l.text(Msg::ReplayMarkOutHover),
                    ConsoleAction::ReplayMarkOut,
                ),
                status.mark_out.is_some().then_some(PREVIEW),
            ),
            (
                ReplayOperation::CaptureMarks { name: None },
                l.text(Msg::ReplayCapture),
                keymap.hint(
                    l,
                    l.text(Msg::ReplayCaptureHover),
                    ConsoleAction::ReplayCapture,
                ),
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
    let selected = selected_event_id(ui)
        .and_then(|id| status.events.iter().find(|candidate| candidate.id == id))
        .or_else(|| status.events.last())
        .is_some_and(|candidate| candidate.id == event.id);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if ui
            .add(key_button(
                l.text(Msg::ReelEventSelect),
                selected.then_some(PREVIEW),
                64.0,
            ))
            .on_hover_text(l.text(Msg::ReelEventSelectHover))
            .clicked()
        {
            select_event(ui, event.id);
        }
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
        // Jog: one frame back or on, paused (Protocol 2.114).
        for (frames, label, hover) in [
            (-1, "◀", Msg::ReplayStepBackHover),
            (1, "▶", Msg::ReplayStepForwardHover),
        ] {
            if ui
                .add_enabled(cued_reason.is_none(), key_button(label, None, 28.0))
                .hint(l.text(hover), reason)
                .clicked()
            {
                intents.push(StudioIntent::Replay(ReplayOperation::Step {
                    input: player.input,
                    frames,
                }));
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
    draw_playlist(ui, l, state, status, player, block, intents);
}

/// The playlist under one Replay input's transport (Protocol 2.119): its
/// entries in order with the current one lit, and ADD CUED, NEXT, PREV, and
/// CLEAR keys.
#[allow(clippy::too_many_lines)] // the entry row and its keys, in one place
fn draw_playlist(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    status: &ReplayStatusMessage,
    player: &ReplayPlayerStatus,
    block: Option<&'static str>,
    intents: &mut Vec<StudioIntent>,
) {
    let input = player.input;
    let empty = player.playlist.is_empty();
    let full = player.playlist.len() >= MAX_REPLAY_PLAYLIST_ENTRIES;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(section_label(l.text(Msg::ReplayPlaylist)));
        ui.label(caption(l.format(
            Msg::ReplayPlaylistCount,
            &[(
                "count",
                Arg::Count(u64::try_from(player.playlist.len()).unwrap_or(u64::MAX)),
            )],
        )));
        // ADD CUED appends the event this player plays, at its angle and
        // speed.
        let add_reason = block.unwrap_or(if player.event.is_none() {
            Msg::ReplayPlaylistReasonNoEvent.en()
        } else {
            Msg::ReplayPlaylistReasonFull.en()
        });
        if ui
            .add_enabled(
                block.is_none() && player.event.is_some() && !full,
                small_button(l.text(Msg::ReplayPlaylistAdd)),
            )
            .hint(l.text(Msg::ReplayPlaylistAddHover), add_reason)
            .clicked()
            && let Some(event) = player.event
        {
            let mut entries = player.playlist.clone();
            entries.push(ReplayPlaylistEntry {
                event,
                angle: player.angle,
                speed: player.speed,
            });
            intents.push(StudioIntent::Replay(ReplayOperation::PlaylistSet {
                input: player.input,
                entries,
            }));
        }
        let empty_reason = block.unwrap_or(Msg::ReplayPlaylistReasonEmpty.en());
        for (operation, label, hover) in [
            (
                ReplayOperation::PlaylistPrev {
                    input: player.input,
                },
                Msg::ReplayPlaylistPrev,
                Msg::ReplayPlaylistPrevHover,
            ),
            (
                ReplayOperation::PlaylistNext {
                    input: player.input,
                },
                Msg::ReplayPlaylistNext,
                Msg::ReplayPlaylistNextHover,
            ),
            (
                ReplayOperation::PlaylistSet {
                    input: player.input,
                    entries: Vec::new(),
                },
                Msg::ReplayPlaylistClear,
                Msg::ReplayPlaylistClearHover,
            ),
        ] {
            if ui
                .add_enabled(block.is_none() && !empty, small_button(l.text(label)))
                .hint(l.text(hover), empty_reason)
                .clicked()
            {
                intents.push(StudioIntent::Replay(operation));
            }
        }
    });
    if empty {
        ui.label(caption(l.text(Msg::ReplayPlaylistEmpty)));
    }
    let count = player.playlist.len();
    for (index, entry) in player.playlist.iter().enumerate() {
        let event = status.events.iter().find(|event| event.id == entry.event);
        let name = event.map_or_else(
            || {
                l.format(
                    Msg::ReplayPlaylistMissingEvent,
                    &[("event", Arg::Plain(u64::from(entry.event)))],
                )
            },
            |event| event.name.clone(),
        );
        let angle = entry
            .angle
            .or_else(|| event.and_then(|event| event.angles.first().copied()))
            .map_or_else(
                || "—".to_owned(),
                |angle| name_of(state, l, angle.to_domain()),
            );
        let current = player.playlist_index == u32::try_from(index).ok();
        let hover = l.format(
            Msg::ReplayPlaylistEntryHover,
            &[
                (
                    "position",
                    Arg::Plain(u64::try_from(index + 1).unwrap_or(u64::MAX)),
                ),
                (
                    "count",
                    Arg::Plain(u64::try_from(count).unwrap_or(u64::MAX)),
                ),
                ("name", Arg::Str(&name)),
                ("angle", Arg::Str(&angle)),
                ("percent", Arg::Plain(u64::from(entry.speed.percent()))),
            ],
        );
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // Reordering keys, then the entry, its angle, its speed, and
            // the remove key.
            for (icon, enabled, hover, reason, target) in [
                (
                    crate::theme::KeyIcon::Up,
                    index > 0,
                    Msg::ReplayPlaylistUpHover,
                    Msg::ReplayPlaylistReasonFirst.en(),
                    index.wrapping_sub(1),
                ),
                (
                    crate::theme::KeyIcon::Down,
                    index + 1 < count,
                    Msg::ReplayPlaylistDownHover,
                    Msg::ReplayPlaylistReasonLast.en(),
                    index + 1,
                ),
            ] {
                let response = add_icon_button(
                    ui,
                    block.is_none() && enabled,
                    key_button("", None, 20.0),
                    icon,
                    l.text(hover),
                )
                .hint(l.text(hover), block.unwrap_or(reason));
                if response.clicked() && target < count {
                    let mut entries = player.playlist.clone();
                    entries.swap(index, target);
                    intents.push(StudioIntent::Replay(ReplayOperation::PlaylistSet {
                        input: player.input,
                        entries,
                    }));
                }
            }
            let label = format!("{}. {name}", index + 1);
            let text = if current {
                RichText::new(label).color(AMBER)
            } else {
                RichText::new(label)
            };
            let hover = if current {
                format!("{}\n{}", l.text(Msg::ReplayPlaylistCurrentHover), hover)
            } else {
                hover
            };
            ui.label(text).on_hover_text(hover);
            ui.label(caption(angle))
                .on_hover_text(l.text(Msg::ReplayAngleHover));
            ComboBox::from_id_salt(("studio-replay-playlist-speed", input, index))
                .selected_text(l.percent(i64::from(entry.speed.percent())))
                .width(56.0)
                .show_ui(ui, |ui| {
                    for speed in SPEEDS {
                        if ui
                            .add_enabled(
                                block.is_none(),
                                egui::Button::selectable(
                                    speed == entry.speed,
                                    l.percent(i64::from(speed.percent())),
                                ),
                            )
                            .hint(l.text(Msg::ReplayPlaylistSpeedHover), block.unwrap_or(""))
                            .clicked()
                            && speed != entry.speed
                        {
                            let mut entries = player.playlist.clone();
                            entries[index].speed = speed;
                            intents.push(StudioIntent::Replay(ReplayOperation::PlaylistSet {
                                input: player.input,
                                entries,
                            }));
                        }
                    }
                })
                .response
                .hint(l.text(Msg::ReplayPlaylistSpeedHover), block.unwrap_or(""));
            let remove = add_icon_button(
                ui,
                block.is_none(),
                key_button("", None, 24.0),
                crate::theme::KeyIcon::Remove,
                l.text(Msg::ReplayPlaylistRemoveHover),
            )
            .hint(l.text(Msg::ReplayPlaylistRemoveHover), block.unwrap_or(""));
            if remove.clicked() {
                let mut entries = player.playlist.clone();
                entries.remove(index);
                intents.push(StudioIntent::Replay(ReplayOperation::PlaylistSet {
                    input: player.input,
                    entries,
                }));
            }
        });
    }
}

/// The REELS section: the reel list, the selected reel's clips, and the
/// export jobs (OR-013).
fn draw_reels(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    status: &ReplayStatusMessage,
    intents: &mut Vec<StudioIntent>,
) {
    ui.label(crate::theme::section_label(l.text(Msg::ReelSection)));
    if state.reels.is_empty() {
        ui.label(caption(l.text(Msg::ReelNoReels)));
    }
    let selected = selected_reel(state, selected_reel_id(ui));
    let event = selected_event(status, selected_event_id(ui));
    let edit = settings_block(state);
    let new_reason = edit
        .or_else(|| (state.reels.len() >= MAX_REELS).then_some(Msg::ReelReasonAllReels.en()))
        .or_else(|| event.is_none().then_some(Msg::ReelReasonNoEvent.en()))
        .or_else(|| {
            event
                .is_some_and(|event| event.angles.is_empty())
                .then_some(Msg::ReelReasonNoAngles.en())
        });
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                new_reason.is_none(),
                key_button(l.text(Msg::ReelNew), None, 82.0),
            )
            .hint(
                l.text(Msg::ReelNewHover),
                new_reason.unwrap_or(Msg::ReasonStateDisallows.en()),
            )
            .clicked()
            && let Some(event) = event
            && let Some(spec) = new_reel(&state.reels, event)
        {
            let id = spec.id;
            intents.push(StudioIntent::Reel(ReelOperation::Set(Box::new(spec))));
            select_reel(ui, id);
        }
    });
    for reel in &state.reels {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            draw_reel_row(
                ui,
                l,
                reel,
                selected.is_some_and(|selected| selected.id == reel.id),
            );
        });
        ui.add_space(4.0);
    }
    if let Some(reel) = selected {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            draw_reel_detail(ui, l, state, status, reel, event, intents);
        });
    }
    ui.add_space(6.0);
    ui.label(crate::theme::section_label(l.text(Msg::ReelExports)));
    if state.reel_exports.is_empty() {
        ui.label(caption(l.text(Msg::ReelNoExports)));
    }
    for export in &state.reel_exports {
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            draw_export(ui, l, state, export, intents);
        });
        ui.add_space(4.0);
    }
}

fn draw_reel_row(ui: &mut Ui, l: Locale, reel: &ReelStatus, selected: bool) {
    ui.horizontal_wrapped(|ui| {
        if ui
            .add(key_button(
                &reel.spec.name,
                selected.then_some(PREVIEW),
                132.0,
            ))
            .on_hover_text(l.text(Msg::ReelSelectHover))
            .clicked()
        {
            select_reel(ui, reel.id);
        }
        ui.label(caption(l.format(
            Msg::ReelClipCount,
            &[(
                "count",
                Arg::Count(u64::try_from(reel.spec.clips.len()).unwrap_or(u64::MAX)),
            )],
        )));
        if let Some(error) = &reel.error {
            status_chip(ui, l.text(Msg::ReelError), Tone::Fault, 100.0)
                .on_hover_text(l.format(Msg::ReelErrorHover, &[("error", Arg::Str(error))]));
        } else if let Some(frames) = reel.total_frames {
            ui.label(mono(
                l.format(Msg::ReelFrames, &[("count", Arg::Count(frames))]),
            ));
        } else {
            ui.label(caption(l.text(Msg::ReelFramesUnknown)));
        }
    });
}

#[allow(clippy::too_many_lines)] // one reel's name, clips, and export
fn draw_reel_detail(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    status: &ReplayStatusMessage,
    reel: &ReelStatus,
    event: Option<&ReplayEventStatus>,
    intents: &mut Vec<StudioIntent>,
) {
    let edit = settings_block(state);
    let reason = edit.unwrap_or(Msg::ReasonStateDisallows.en());
    ui.horizontal_wrapped(|ui| {
        ui.label(l.text(Msg::ReelName));
        let id = Id::new(("studio-reel-name", reel.id));
        let mut name = ui
            .data(|data| data.get_temp::<String>(id))
            .unwrap_or_else(|| reel.spec.name.clone());
        let response = ui
            .add_enabled(
                edit.is_none(),
                TextEdit::singleline(&mut name).desired_width(150.0),
            )
            .hint(l.text(Msg::ReelNameHover), reason);
        if response.has_focus() {
            ui.data_mut(|data| data.insert_temp(id, name.clone()));
        } else {
            ui.data_mut(|data| data.remove::<String>(id));
        }
        if response.lost_focus()
            && let Some(operation) = rename_reel(reel, &name)
        {
            intents.push(StudioIntent::Reel(operation));
        }
        if pending_delete(ui) != Some(reel.id)
            && ui
                .add_enabled(
                    edit.is_none(),
                    key_button(l.text(Msg::ReelDelete), None, 68.0),
                )
                .hint(l.text(Msg::ReelDeleteHover), reason)
                .clicked()
        {
            set_pending_delete(ui, Some(reel.id));
        }
    });
    if pending_delete(ui) == Some(reel.id) {
        ui.horizontal_wrapped(|ui| {
            ui.label(l.format(
                Msg::ReelDeleteQuestion,
                &[("name", Arg::Str(&reel.spec.name))],
            ));
            if ui
                .add_enabled(
                    edit.is_none(),
                    key_button(l.text(Msg::ReelDelete), Some(PROGRAM), 68.0),
                )
                .hint(l.text(Msg::ReelDeleteConfirmHover), reason)
                .clicked()
            {
                intents.push(StudioIntent::Reel(ReelOperation::Delete { reel: reel.id }));
                set_pending_delete(ui, None);
            }
            if ui
                .add(key_button(l.text(Msg::CancelKey), None, 68.0))
                .on_hover_text(l.text(Msg::ReelDeleteCancelHover))
                .clicked()
            {
                set_pending_delete(ui, None);
            }
        });
    }
    ui.add_space(4.0);
    ui.label(caption(l.text(Msg::ReelClips)));
    if reel.spec.clips.is_empty() {
        ui.label(caption(l.text(Msg::ReelNoClips)));
    }
    for (index, clip) in reel.spec.clips.iter().enumerate() {
        draw_clip(ui, l, state, status, reel, index, *clip, edit, intents);
        ui.add_space(2.0);
    }
    let add_reason = edit
        .or_else(|| {
            (reel.spec.clips.len() >= MAX_REEL_CLIPS).then_some(Msg::ReelReasonFullClips.en())
        })
        .or_else(|| event.is_none().then_some(Msg::ReelReasonNoEvent.en()))
        .or_else(|| {
            event
                .is_some_and(|event| event.angles.is_empty())
                .then_some(Msg::ReelReasonNoAngles.en())
        });
    if ui
        .add_enabled(
            add_reason.is_none(),
            key_button(l.text(Msg::ReelAddEvent), None, 86.0),
        )
        .hint(
            l.text(Msg::ReelAddEventHover),
            add_reason.unwrap_or(Msg::ReasonStateDisallows.en()),
        )
        .clicked()
        && let Some(event) = event
        && let Some(operation) = add_event_clip(reel, event)
    {
        intents.push(StudioIntent::Reel(operation));
    }
    draw_music(ui, l, reel, edit, intents);
    draw_export_controls(ui, l, state, reel, intents);
}

/// The reel's music bed: its file, and its level and ducked level while
/// one is set (schema 86).
fn draw_music(
    ui: &mut Ui,
    l: Locale,
    reel: &ReelStatus,
    edit: Option<&'static str>,
    intents: &mut Vec<StudioIntent>,
) {
    let reason = edit.unwrap_or(Msg::ReasonStateDisallows.en());
    ui.add_space(4.0);
    ui.label(caption(l.text(Msg::ReelMusicSection)));
    ui.horizontal_wrapped(|ui| {
        ui.label(l.text(Msg::ReelDestination));
        let id = Id::new(("studio-reel-music-path", reel.id));
        let current = reel
            .spec
            .music
            .as_ref()
            .map(|music| music.path.clone())
            .unwrap_or_default();
        let mut path = ui
            .data(|data| data.get_temp::<String>(id))
            .unwrap_or_else(|| current.clone());
        let response = ui
            .add_enabled(
                edit.is_none(),
                TextEdit::singleline(&mut path).desired_width(180.0),
            )
            .hint(l.text(Msg::ReelMusicPathHover), reason);
        if response.changed() {
            ui.data_mut(|data| data.insert_temp(id, path.clone()));
        }
        let bed = music_with_path(reel, &path);
        let set_reason = edit
            .or_else(|| (!bed.is_valid()).then_some(Msg::ReelReasonInvalidPath.en()))
            .or_else(|| {
                (reel.spec.music.as_ref() == Some(&bed)).then_some(Msg::ReelReasonMusicSet.en())
            });
        if ui
            .add_enabled(
                set_reason.is_none(),
                key_button(l.text(Msg::ReelMusicSet), None, 52.0),
            )
            .hint(
                l.text(Msg::ReelMusicSetHover),
                set_reason.unwrap_or(Msg::ReasonStateDisallows.en()),
            )
            .clicked()
            && let Some(operation) = set_music(reel, Some(bed))
        {
            ui.data_mut(|data| data.remove::<String>(id));
            intents.push(StudioIntent::Reel(operation));
        }
        let remove_reason = edit.or_else(|| {
            reel.spec
                .music
                .is_none()
                .then_some(Msg::ReelReasonNoMusic.en())
        });
        if ui
            .add_enabled(
                remove_reason.is_none(),
                key_button(l.text(Msg::ReelMusicRemove), None, 100.0),
            )
            .hint(
                l.text(Msg::ReelMusicRemoveHover),
                remove_reason.unwrap_or(Msg::ReasonStateDisallows.en()),
            )
            .clicked()
            && let Some(operation) = set_music(reel, None)
        {
            ui.data_mut(|data| data.remove::<String>(id));
            intents.push(StudioIntent::Reel(operation));
        }
    });
    let Some(music) = &reel.spec.music else {
        ui.label(caption(l.text(Msg::ReelMusicNone)));
        return;
    };
    ui.horizontal_wrapped(|ui| {
        for level in [MusicLevel::Level, MusicLevel::Ducked] {
            ui.label(l.text(level.label()));
            let value = level.value(music);
            let id = Id::new(("studio-reel-music-level", reel.id, level));
            let mut percent = ui.data(|data| data.get_temp::<u8>(id)).unwrap_or(value);
            let response = ui
                .add_enabled(
                    edit.is_none(),
                    DragValue::new(&mut percent)
                        .range(0..=MAX_REEL_MUSIC_PERCENT)
                        .suffix(" %"),
                )
                .hint(l.text(level.hover()), reason);
            if response.dragged() || response.has_focus() {
                ui.data_mut(|data| data.insert_temp(id, percent));
            } else {
                ui.data_mut(|data| data.remove::<u8>(id));
            }
            if (response.drag_stopped() || response.lost_focus()) && percent != value {
                let mut updated = music.clone();
                level.set(&mut updated, percent);
                if let Some(operation) = set_music(reel, Some(updated)) {
                    intents.push(StudioIntent::Reel(operation));
                }
            }
        }
    });
}

/// One of a music bed's two levels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum MusicLevel {
    Level,
    Ducked,
}

impl MusicLevel {
    const fn value(self, music: &ReelMusic) -> u8 {
        match self {
            Self::Level => music.level_percent,
            Self::Ducked => music.duck_percent,
        }
    }

    const fn set(self, music: &mut ReelMusic, percent: u8) {
        match self {
            Self::Level => music.level_percent = percent,
            Self::Ducked => music.duck_percent = percent,
        }
    }

    const fn label(self) -> Msg {
        match self {
            Self::Level => Msg::ReelMusicLevel,
            Self::Ducked => Msg::ReelMusicDucked,
        }
    }

    const fn hover(self) -> Msg {
        match self {
            Self::Level => Msg::ReelMusicLevelHover,
            Self::Ducked => Msg::ReelMusicDuckedHover,
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // one clip's angle, trims, speed, and keys
fn draw_clip(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    status: &ReplayStatusMessage,
    reel: &ReelStatus,
    index: usize,
    clip: ReelClip,
    edit: Option<&'static str>,
    intents: &mut Vec<StudioIntent>,
) {
    let event = status.events.iter().find(|event| event.id == clip.event);
    let name = event.map_or_else(|| format!("#{}", clip.event), |event| event.name.clone());
    let reason = edit.unwrap_or(Msg::ReasonStateDisallows.en());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(mono(l.format(
            Msg::ReelClipLabel,
            &[(
                "position",
                Arg::Count(u64::try_from(index + 1).unwrap_or(u64::MAX)),
            )],
        )))
        .on_hover_text(l.text(Msg::ReelClipEventHover));
        ui.label(caption(name))
            .on_hover_text(l.text(Msg::ReelClipEventHover));
        let mut angles = event.map(|event| event.angles.clone()).unwrap_or_default();
        if !angles.iter().any(|angle| angle.to_domain() == clip.angle) {
            angles.insert(0, fm_protocol::WireInputId::from_domain(clip.angle));
        }
        ComboBox::from_id_salt(("studio-reel-angle", reel.id, index))
            .selected_text(name_of(state, l, clip.angle))
            .width(110.0)
            .show_ui(ui, |ui| {
                for angle in &angles {
                    if ui
                        .add_enabled(
                            edit.is_none(),
                            egui::Button::selectable(
                                angle.to_domain() == clip.angle,
                                name_of(state, l, angle.to_domain()),
                            ),
                        )
                        .hint(l.text(Msg::ReelClipAngleHover), reason)
                        .clicked()
                        && angle.to_domain() != clip.angle
                    {
                        let mut updated = clip;
                        updated.angle = angle.to_domain();
                        if let Some(operation) = set_clip(reel, index, updated) {
                            intents.push(StudioIntent::Reel(operation));
                        }
                    }
                }
            })
            .response
            .on_hover_text(l.text(Msg::ReelClipAngleHover));
        ui.label(caption(l.text(Msg::MediaTrimIn)))
            .on_hover_text(l.text(Msg::ReelClipTrimInHover));
        draw_frames(ui, l, reel, index, clip, edit, ClipFrames::TrimIn, intents);
        ui.label(caption(l.text(Msg::MediaTrimOut)))
            .on_hover_text(l.text(Msg::ReelClipTrimOutHover));
        draw_frames(ui, l, reel, index, clip, edit, ClipFrames::TrimOut, intents);
        ComboBox::from_id_salt(("studio-reel-speed", reel.id, index))
            .selected_text(l.percent(i64::from(clip.speed_percent)))
            .width(66.0)
            .show_ui(ui, |ui| {
                for speed in REEL_SPEEDS {
                    if ui
                        .add_enabled(
                            edit.is_none(),
                            egui::Button::selectable(
                                speed == clip.speed_percent,
                                l.percent(i64::from(speed)),
                            ),
                        )
                        .hint(l.text(Msg::ReelClipSpeedHover), reason)
                        .clicked()
                        && speed != clip.speed_percent
                    {
                        let mut updated = clip;
                        updated.speed_percent = speed;
                        if let Some(operation) = set_clip(reel, index, updated) {
                            intents.push(StudioIntent::Reel(operation));
                        }
                    }
                }
            })
            .response
            .on_hover_text(l.text(Msg::ReelClipSpeedHover));
        ui.label(caption(l.text(Msg::ReelClipDissolve)))
            .on_hover_text(l.text(Msg::ReelClipDissolveHover));
        // The first clip always cuts in.
        let dissolve_edit = edit.or((index == 0).then_some(Msg::ReelReasonFirstClipCuts.en()));
        draw_frames(
            ui,
            l,
            reel,
            index,
            clip,
            dissolve_edit,
            ClipFrames::Dissolve,
            intents,
        );
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let up_reason = edit.or((index == 0).then_some(Msg::ReelReasonFirstClip.en()));
        if ui
            .add_enabled(
                up_reason.is_none(),
                key_button(l.text(Msg::KeyMoveUp), None, 62.0),
            )
            .hint(
                l.text(Msg::ReelClipMoveUpHover),
                up_reason.unwrap_or(Msg::ReasonStateDisallows.en()),
            )
            .clicked()
            && let Some(operation) = move_clip(reel, index, true)
        {
            intents.push(StudioIntent::Reel(operation));
        }
        let down_reason =
            edit.or((index + 1 == reel.spec.clips.len()).then_some(Msg::ReelReasonLastClip.en()));
        if ui
            .add_enabled(
                down_reason.is_none(),
                key_button(l.text(Msg::KeyMoveDown), None, 62.0),
            )
            .hint(
                l.text(Msg::ReelClipMoveDownHover),
                down_reason.unwrap_or(Msg::ReasonStateDisallows.en()),
            )
            .clicked()
            && let Some(operation) = move_clip(reel, index, false)
        {
            intents.push(StudioIntent::Reel(operation));
        }
        let remove_reason =
            edit.or((reel.spec.clips.len() <= 1).then_some(Msg::ReelReasonOnlyClip.en()));
        if ui
            .add_enabled(
                remove_reason.is_none(),
                key_button(l.text(Msg::ReelClipRemove), None, 86.0),
            )
            .hint(
                l.text(Msg::ReelClipRemoveHover),
                remove_reason.unwrap_or(Msg::ReasonStateDisallows.en()),
            )
            .clicked()
            && let Some(operation) = remove_clip(reel, index)
        {
            intents.push(StudioIntent::Reel(operation));
        }
    });
}

/// A clip's frame-count fields.
#[derive(Clone, Copy, Debug, Hash)]
enum ClipFrames {
    TrimIn,
    TrimOut,
    /// Frames the clip dissolves in over the clip before (schema 84).
    Dissolve,
}

impl ClipFrames {
    const fn value(self, clip: ReelClip) -> u32 {
        match self {
            Self::TrimIn => clip.trim_in_frames,
            Self::TrimOut => clip.trim_out_frames,
            Self::Dissolve => clip.dissolve_frames,
        }
    }

    const fn set(self, clip: &mut ReelClip, frames: u32) {
        match self {
            Self::TrimIn => clip.trim_in_frames = frames,
            Self::TrimOut => clip.trim_out_frames = frames,
            Self::Dissolve => clip.dissolve_frames = frames,
        }
    }

    const fn maximum(self) -> u32 {
        match self {
            Self::TrimIn | Self::TrimOut => MAX_REEL_TRIM_FRAMES,
            Self::Dissolve => MAX_REEL_DISSOLVE_FRAMES,
        }
    }

    const fn hover(self) -> Msg {
        match self {
            Self::TrimIn => Msg::ReelClipTrimInHover,
            Self::TrimOut => Msg::ReelClipTrimOutHover,
            Self::Dissolve => Msg::ReelClipDissolveHover,
        }
    }
}

#[allow(clippy::too_many_arguments)] // one frame field's draft and its reel clip
fn draw_frames(
    ui: &mut Ui,
    l: Locale,
    reel: &ReelStatus,
    index: usize,
    clip: ReelClip,
    edit: Option<&'static str>,
    field: ClipFrames,
    intents: &mut Vec<StudioIntent>,
) {
    let value = field.value(clip);
    let id = Id::new(("studio-reel-frames", reel.id, index, field));
    let mut frames = ui.data(|data| data.get_temp::<u32>(id)).unwrap_or(value);
    let hover = field.hover();
    let response = ui
        .add_enabled(
            edit.is_none(),
            DragValue::new(&mut frames)
                .range(0..=field.maximum())
                .suffix(" fr"),
        )
        .hint(
            l.text(hover),
            edit.unwrap_or(Msg::ReasonStateDisallows.en()),
        );
    if response.dragged() || response.has_focus() {
        ui.data_mut(|data| data.insert_temp(id, frames));
    } else {
        ui.data_mut(|data| data.remove::<u32>(id));
    }
    if (response.drag_stopped() || response.lost_focus()) && frames != value {
        let mut updated = clip;
        field.set(&mut updated, frames);
        if let Some(operation) = set_clip(reel, index, updated) {
            intents.push(StudioIntent::Reel(operation));
        }
    }
}

fn draw_export_controls(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    reel: &ReelStatus,
    intents: &mut Vec<StudioIntent>,
) {
    let live = live_block(state);
    let reason = live.unwrap_or(Msg::ReasonStateDisallows.en());
    ui.add_space(4.0);
    ui.label(caption(l.text(Msg::ReelExportSection)));
    ui.horizontal_wrapped(|ui| {
        ui.label(l.text(Msg::ReelDestination));
        let id = reel_destination_id(reel.id);
        let mut destination = ui
            .data(|data| data.get_temp::<String>(id))
            .unwrap_or_default();
        let response = ui
            .add_enabled(
                live.is_none(),
                TextEdit::singleline(&mut destination).desired_width(180.0),
            )
            .hint(l.text(Msg::ReelDestinationHover), reason);
        if response.changed() {
            ui.data_mut(|data| data.insert_temp(id, destination.clone()));
        }
        let active = state
            .reel_exports
            .iter()
            .any(|export| export_is_active(export.state));
        let job = next_export_job(&state.reel_exports);
        let export_reason = live
            .or_else(|| active.then_some(Msg::ReelReasonExportActive.en()))
            .or_else(|| {
                reel.spec
                    .clips
                    .is_empty()
                    .then_some(Msg::ReelReasonNoClips.en())
            })
            .or_else(|| {
                reel.error
                    .is_some()
                    .then_some(Msg::ReelReasonReelError.en())
            })
            .or_else(|| {
                (!reel_export_path_is_valid(&destination))
                    .then_some(Msg::ReelReasonInvalidPath.en())
            })
            .or_else(|| job.is_none().then_some(Msg::ReelReasonExportJobsFull.en()));
        if ui
            .add_enabled(
                export_reason.is_none(),
                key_button(l.text(Msg::ReelExport), None, 72.0),
            )
            .hint(
                l.text(Msg::ReelExportHover),
                export_reason.unwrap_or(Msg::ReasonStateDisallows.en()),
            )
            .clicked()
            && let Some(job) = job
            && let Some(operation) = export_reel(reel, job, &destination)
        {
            intents.push(StudioIntent::Reel(operation));
        }
    });
}

fn draw_export(
    ui: &mut Ui,
    l: Locale,
    state: &StudioUiState,
    export: &ReelExportStatus,
    intents: &mut Vec<StudioIntent>,
) {
    let live = live_block(state);
    let reason = live.unwrap_or(Msg::ReasonStateDisallows.en());
    let reel = state
        .reels
        .iter()
        .find(|reel| reel.id == export.reel)
        .map_or_else(
            || format!("#{}", export.reel),
            |reel| reel.spec.name.clone(),
        );
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(mono(
            l.format(Msg::ReelExportJob, &[("job", Arg::Count(export.job))]),
        ));
        ui.label(caption(reel))
            .on_hover_text(l.text(Msg::ReelExportReelHover));
        status_chip(
            ui,
            l.text(export_state_msg(export.state)),
            export_tone(export.state),
            120.0,
        );
        let total = export
            .total_frames
            .map_or_else(|| "--".to_owned(), |total| total.to_string());
        let completed = export.completed_frames;
        ui.label(mono(format!("{completed}/{total}")))
            .on_hover_text(l.text(Msg::ReelExportProgressHover));
        if let Some(bytes) = export.bytes {
            ui.label(mono(l.bytes(bytes)))
                .on_hover_text(l.text(Msg::ReelExportBytesHover));
        }
        if let Some(failure) = &export.failure {
            ui.label(caption(failure.as_str()))
                .on_hover_text(l.text(Msg::ReelExportFailureHover));
        }
        if export_is_active(export.state) {
            if ui
                .add_enabled(
                    live.is_none(),
                    key_button(l.text(Msg::ReelExportCancel), None, 96.0),
                )
                .hint(l.text(Msg::ReelExportCancelHover), reason)
                .clicked()
            {
                intents.push(StudioIntent::Reel(ReelOperation::CancelExport {
                    job: export.job,
                }));
            }
        } else if ui
            .add_enabled(
                live.is_none(),
                key_button(l.text(Msg::ReelExportForget), None, 76.0),
            )
            .hint(l.text(Msg::ReelExportForgetHover), reason)
            .clicked()
        {
            intents.push(StudioIntent::Reel(ReelOperation::ForgetExport {
                job: export.job,
            }));
        }
    });
    ui.label(caption(export.destination.as_str()))
        .on_hover_text(l.text(Msg::ReelExportDestinationHover));
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_ui_model::i18n::Language;

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
            auto_return: None,
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

    #[test]
    fn set_reel_status_sorts_reels_and_replaces_exports() {
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready);
        let mut status = crate::hover_audit::reel_status();
        status.reels.reverse();
        state.set_reel_status(status);
        assert_eq!(
            state.reels.iter().map(|reel| reel.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(state.reel_exports.len(), 6);
        let mut next = crate::hover_audit::reel_status();
        next.reels.clear();
        next.exports.clear();
        state.set_reel_status(next);
        assert!(state.reels.is_empty());
        assert!(state.reel_exports.is_empty());
    }

    /// OR-013: each reel key builds exactly its Set/Export command.
    #[test]
    fn music_keys_set_change_and_remove_the_bed() {
        let status = crate::hover_audit::reel_status();
        let scored = &status.reels[0];
        let bed = scored.spec.music.clone().expect("the goal reel has music");
        // A new file keeps the bed's levels; the same file changes nothing.
        let moved = music_with_path(scored, " /srv/music/other.wav ");
        assert_eq!(moved.path, "/srv/music/other.wav");
        assert_eq!(
            (moved.level_percent, moved.duck_percent),
            (bed.level_percent, bed.duck_percent)
        );
        assert_eq!(
            set_music(scored, Some(moved.clone())),
            Some(ReelOperation::Set(Box::new(ReelSpec {
                music: Some(moved),
                ..scored.spec.clone()
            })))
        );
        assert_eq!(set_music(scored, Some(bed)), None);
        assert_eq!(
            set_music(scored, None),
            Some(ReelOperation::Set(Box::new(ReelSpec {
                music: None,
                ..scored.spec.clone()
            })))
        );
        // A reel without music takes the default levels, and refuses a
        // relative path.
        let silent = &status.reels[1];
        let fresh = music_with_path(silent, "/srv/music/bed.wav");
        assert_eq!(
            (fresh.level_percent, fresh.duck_percent),
            (DEFAULT_MUSIC_LEVEL, DEFAULT_MUSIC_DUCK)
        );
        assert_eq!(set_music(silent, None), None);
        assert_eq!(
            set_music(silent, Some(music_with_path(silent, "music/bed.wav"))),
            None
        );
    }

    #[test]
    fn reel_keys_build_the_expected_reel_operations() {
        let status = crate::hover_audit::reel_status();
        let replay = crate::hover_audit::replay_status();
        let event = &replay.events[0];
        let reels = &status.reels[..1];
        let spec = new_reel(reels, event).expect("the second id is free");
        assert_eq!(spec.id, 2);
        assert_eq!(spec.name, "Reel 2");
        assert_eq!(
            spec.clips,
            vec![ReelClip {
                event: 1,
                angle: input(1),
                trim_in_frames: 0,
                trim_out_frames: 0,
                speed_percent: 100,
                dissolve_frames: 0,
            }]
        );

        let reel = &status.reels[0];
        let Some(ReelOperation::Set(spec)) = add_event_clip(reel, event) else {
            panic!("Add event must set the reel");
        };
        assert_eq!(spec.clips.len(), reel.spec.clips.len() + 1);
        assert_eq!(spec.clips.last().map(|clip| clip.event), Some(event.id));
        assert_eq!(spec.clips.last().map(|clip| clip.speed_percent), Some(100));

        // The second clip dissolves in; whichever clip becomes first cuts.
        assert_eq!(reel.spec.clips[1].dissolve_frames, 8);
        let cut = ReelClip {
            dissolve_frames: 0,
            ..reel.spec.clips[1]
        };
        let Some(ReelOperation::Set(spec)) = move_clip(reel, 0, false) else {
            panic!("Move down must set the reel");
        };
        assert_eq!(spec.clips[0], cut);
        assert_eq!(spec.clips[1], reel.spec.clips[0]);

        let Some(ReelOperation::Set(spec)) = remove_clip(reel, 0) else {
            panic!("Remove clip must set the reel");
        };
        assert_eq!(spec.clips, vec![cut]);

        assert_eq!(
            rename_reel(reel, "Goals"),
            Some(ReelOperation::Set(Box::new(ReelSpec {
                name: "Goals".into(),
                ..reel.spec.clone()
            })))
        );
        assert_eq!(rename_reel(reel, &reel.spec.name), None);

        assert_eq!(
            export_reel(reel, 1, "/srv/exports/reel-1.mp4"),
            Some(ReelOperation::Export {
                job: 1,
                reel: 1,
                destination: "/srv/exports/reel-1.mp4".into(),
            })
        );
        assert_eq!(export_reel(reel, 1, "relative.mp4"), None);
        assert_eq!(next_export_job(&status.exports), Some(7));
    }

    /// A viewer sends no reel edit, a locked project keeps its live export
    /// keys, and drawing the panel itself sends nothing.
    #[test]
    fn reel_edits_follow_the_padlock_while_exports_stay_live() {
        let view = crate::hover_audit::bank_fixture().view.unwrap();
        let ready = |transition: bool, edit: bool| {
            StudioUiState::new(StudioConnectionStatus::Ready)
                .with_view(view.clone())
                .with_switcher_permissions(transition, transition)
                .with_edit_project_permission(edit)
        };
        let viewer = ready(false, false);
        assert!(settings_block(&viewer).is_some());
        assert!(live_block(&viewer).is_some());
        let mut locked = ready(true, true);
        if let Some(view) = locked.view.as_mut() {
            view.edit_locks.project = true;
        }
        assert_eq!(
            settings_block(&locked),
            Some(crate::edit_history::PROJECT_LOCKED_REASON)
        );
        // Export control is a live operation: the padlock does not block it.
        assert_eq!(live_block(&locked), None);

        let render = |state: &StudioUiState| {
            let context = egui::Context::default();
            let mut intents = Vec::new();
            let _ = context.run_ui(egui::RawInput::default(), |ui| {
                draw_replay(ui, state, &mut intents);
            });
            intents
        };
        let mut state = locked;
        state.replay = Some(Box::new(crate::hover_audit::replay_status()));
        state.set_reel_status(crate::hover_audit::reel_status());
        assert_eq!(render(&state), Vec::new());
        let mut viewer = viewer;
        viewer.replay = Some(Box::new(crate::hover_audit::replay_status()));
        viewer.set_reel_status(crate::hover_audit::reel_status());
        assert_eq!(render(&viewer), Vec::new());
        assert_eq!(
            render(&StudioUiState::new(StudioConnectionStatus::Ready)),
            Vec::new()
        );
    }

    /// OR-013: a row in every export state draws without panicking.
    #[test]
    fn every_export_state_draws_without_panicking() {
        let mut state = crate::hover_audit::bank_fixture();
        state.replay = Some(Box::new(crate::hover_audit::replay_status()));
        state.set_reel_status(crate::hover_audit::reel_status());
        assert_eq!(state.reel_exports.len(), 6);
        let context = egui::Context::default();
        let mut intents = Vec::new();
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            draw_replay(ui, &state, &mut intents);
        });
        assert_eq!(intents, Vec::new());
    }

    /// OR-013: the REELS and export sections render in every language.
    #[test]
    fn the_reels_section_draws_in_english_and_spanish() {
        fn painted_text(output: &egui::FullOutput) -> Vec<String> {
            fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
                match shape {
                    egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
                    egui::Shape::Vec(shapes) => {
                        for shape in shapes {
                            collect(shape, out);
                        }
                    }
                    _ => {}
                }
            }
            let mut out = Vec::new();
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut out);
            }
            out
        }
        for language in Language::ALL {
            let locale = Locale::new(language);
            let mut state = crate::hover_audit::bank_fixture();
            state.replay = Some(Box::new(crate::hover_audit::replay_status()));
            state.set_reel_status(crate::hover_audit::reel_status());
            let context = egui::Context::default();
            crate::l10n::set_locale(&context, locale);
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::Vec2::new(1_600.0, 2_400.0),
                    )),
                    ..egui::RawInput::default()
                },
                |ui| {
                    let mut intents = Vec::new();
                    draw_replay(ui, &state, &mut intents);
                    assert!(intents.is_empty());
                },
            );
            let text = painted_text(&output);
            for message in [Msg::ReelSection, Msg::ReelExports] {
                let expected = locale.text(message).to_uppercase();
                assert!(
                    text.iter().any(|line| line == &expected),
                    "{language:?}: {expected:?} missing from {text:?}"
                );
            }
        }
    }
}
