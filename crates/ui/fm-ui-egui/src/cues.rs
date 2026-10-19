//! Cues side tab: the operator cue lists (`RC-017`).
//!
//! The tab shows one replicated cue list ([`fm_ui_model::ProjectState::cue_lists`])
//! at a time: a big GO key that fires the cue on standby, the standby cue's
//! name, the Back, Skip, Stop, and Reset keys, and every cue with its run
//! state (live, standby, waiting, played, or stopped by a refused action).
//! GO, standby (a row's standby key), and fire-now (a row's fire key) need the
//! transition permission; `G` presses GO from anywhere in the console.
//!
//! Arm GO (Protocol 2.64) arms the cue on standby to fire by itself after a
//! delay typed beside it (seconds, `MM:SS`, or `HH:MM:SS`, up to 24 hours),
//! with GO's gates; Disarm cancels it. The armed cue's chip reads ARMED and
//! a note counts it down, extrapolated at the show rate from the newest
//! frame a cue record reported ([`ProjectState::cue_frame`]), so the
//! countdown is approximate; the server's clock fires it on its frame.
//!
//! Editing (lists, cues, and actions, all inline) needs `edit_project`, an
//! unlocked project, and no cue of the list waiting on its next actions,
//! because the engine refuses to change a running cue. The tab edits a
//! working copy of the list: a discrete change (a picker, a checkbox, a
//! key) sends the whole list at once, while a text field sends it when it
//! loses focus and a number when its drag ends, so each edit is one
//! undoable step. The copy follows the replicated list whenever another
//! client changes it, and keeps an edit this console sent until its echo
//! lands; an edit refused (resolved without an echo) snaps back.

use egui::{Button, ComboBox, DragValue, Frame, RichText, Stroke, TextEdit, Ui, Vec2};
use std::time::{Duration, Instant};

use fm_protocol::{
    AutoTransitionKind, CueFaultStatus, CueListSpec, CueListStatus, CueOperation, CueRunStatus,
    MAX_CUE_ACTIONS, MAX_CUE_ARM_MS, MAX_CUE_DELAY_MS, MAX_CUE_LIST_SCRIPT_STEPS, MAX_CUE_LISTS,
    MAX_CUE_TRANSITION_FRAMES, MAX_CUE_WAIT_FRAMES, MAX_CUES, ScriptStatus,
    TransitionFavoriteEffect, WireCue, WireCueAction, WireCueMediaOperation, WireInputId,
    WireOverlayChannelId, WireRecorderSlot, WireStingerSlotId, WireStreamTargetId,
};
use fm_types::InputId;
use fm_ui_model::{ClientView, ProjectState, ScriptVerb, script_verbs};

use crate::edit_history::stinger_edit_lock_reason;
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::l10n::{self, Arg, Locale, Msg};
use crate::monitors::input_name;
use crate::playlist::{countdown_timecode, estimated_frame, key_button};
use crate::theme::{
    ERROR, KeyIcon, PREVIEW, PROGRAM, Tone, add_icon_button, border_color, caption, empty_state,
    field_label, legible, raised_color, small_button, status_chip,
};
use crate::{StudioConnectionStatus, StudioIntent, StudioUiState};

/// The strip gain range the engine accepts, in millidecibels.
const MIN_GAIN_MILLIDB: i32 = -96_000;
const MAX_GAIN_MILLIDB: i32 = 24_000;

/// Every cue that would take Program a second time on one frame, with the
/// cue whose take it collides with: `(offending cue, earlier taker)`, the
/// two equal when one cue takes twice between waits. The actions up to a
/// cue's first wait run on one frame, and a cue that auto-follows without
/// a trailing wait carries its last frame into the next cue, so the engine
/// refuses such a list; the editor holds it as a draft and says why.
#[must_use]
pub fn take_conflicts(cues: &[WireCue]) -> Vec<(u64, u64)> {
    let mut conflicts = Vec::new();
    let mut carried = None;
    for cue in cues {
        let mut taker = carried;
        let mut conflict = None;
        for action in &cue.actions {
            match action {
                WireCueAction::Wait { .. } | WireCueAction::Delay { .. } => taker = None,
                WireCueAction::Take { .. } | WireCueAction::PlaylistStart => match taker {
                    Some(earlier) => {
                        conflict.get_or_insert(earlier);
                    }
                    None => taker = Some(cue.id),
                },
                _ => {}
            }
        }
        if let Some(earlier) = conflict {
            conflicts.push((cue.id, earlier));
        }
        carried = if cue.auto_follow { taker } else { None };
    }
    conflicts
}

/// A cue list operator action. Studio maps each to one protocol command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CueIntent {
    /// Creates or replaces one list (`cue_list_set`): an undoable project
    /// edit.
    Set { list: u64, spec: CueListSpec },
    /// Deletes one list (`cue_list_remove`): an undoable project edit.
    Remove { list: u64 },
    /// GO, standby, back, skip, stop, reset, arm, or disarm (`cue_go`,
    /// `cue_standby`, ..., `cue_arm`, `cue_disarm`): a journaled operator
    /// control, not undoable.
    Control { list: u64, operation: CueOperation },
}

/// The note under a list whose deferred action was refused: where its cue
/// stopped and, since Protocol 2.63, what the server refused it with.
#[must_use]
pub(crate) fn fault_note(l: Locale, state: &StudioUiState, fault: &CueFaultStatus) -> String {
    let (cue, action) = (
        fault.cue.to_string(),
        (u32::from(fault.action) + 1).to_string(),
    );
    let mut text = l.format(
        Msg::CueFaultNote,
        &[("cue", Arg::Str(&cue)), ("action", Arg::Str(&action))],
    );
    text.push(' ');
    text.push_str(&crate::scripts::fault_detail_text(l, state, &fault.detail));
    text
}

/// Why cue controls (GO and the transport keys) are unavailable for the
/// session, if they are.
#[must_use]
pub fn control_block(state: &StudioUiState) -> Option<&'static str> {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.view.is_some()
        && state.can_transition;
    (!ready).then(|| unavailable_reason(state, Capability::Transition))
}

/// Why lists cannot be created or deleted, if they cannot: the session
/// gates, then the project padlock.
#[must_use]
pub fn edit_block(state: &StudioUiState) -> Option<&'static str> {
    let ready = state.connection_status == StudioConnectionStatus::Ready
        && state.view.is_some()
        && state.can_edit_project;
    if !ready {
        return Some(unavailable_reason(state, Capability::EditProject));
    }
    stinger_edit_lock_reason(state)
}

/// Why `list` cannot be edited, if it cannot: [`edit_block`], then a cue
/// waiting on its next actions.
#[must_use]
pub fn list_edit_block(state: &StudioUiState, list: &CueListStatus) -> Option<&'static str> {
    edit_block(state).or_else(|| {
        list.state
            .pending
            .is_some()
            .then_some(Msg::CueReasonRunning.en())
    })
}

/// The permission an action needs beyond the `transition` every cue
/// control needs: the permission of the equivalent direct command. A
/// script step needs those of its script's verbs ([`verb_capability`]).
const fn action_capability(action: &WireCueAction) -> Option<Capability> {
    match action {
        WireCueAction::Preview { .. } => Some(Capability::SelectPreview),
        WireCueAction::AudioGain { .. } | WireCueAction::AudioMute { .. } => {
            Some(Capability::Audio)
        }
        WireCueAction::TitleText { .. } => Some(Capability::EditProject),
        _ => None,
    }
}

/// The permission a script action of `verb` needs beyond `transition`.
const fn verb_capability(verb: ScriptVerb) -> Option<Capability> {
    match verb {
        ScriptVerb::Preview => Some(Capability::SelectPreview),
        ScriptVerb::Gain | ScriptVerb::Mute | ScriptVerb::Unmute => Some(Capability::Audio),
        ScriptVerb::TitleText => Some(Capability::EditProject),
        _ => None,
    }
}

/// Why this session may not fire cue `cue` of `list`, if it may not. A GO
/// runs the cue and every cue it auto-follows into, and the authority
/// checks that whole chain up front: each action needs the permission its
/// equivalent direct command needs, and a script step (of `scripts`) the
/// permission of every action its script holds, in any branch.
#[must_use]
pub fn chain_block(
    state: &StudioUiState,
    scripts: &[ScriptStatus],
    list: &CueListStatus,
    cue: u64,
) -> Option<&'static str> {
    let start = list.spec.cues.iter().position(|found| found.id == cue)?;
    let (mut preview, mut audio, mut title) = (false, false, false);
    for found in &list.spec.cues[start..] {
        for action in &found.actions {
            let needs: Vec<Option<Capability>> = match action {
                WireCueAction::RunScript { script } => scripts
                    .iter()
                    .find(|candidate| candidate.id == *script)
                    .map(|found| script_verbs(&found.spec.source))
                    .unwrap_or_default()
                    .into_iter()
                    .map(verb_capability)
                    .collect(),
                action => vec![action_capability(action)],
            };
            for need in needs {
                match need {
                    Some(Capability::SelectPreview) => preview = true,
                    Some(Capability::Audio) => audio = true,
                    Some(Capability::EditProject) => title = true,
                    Some(Capability::Transition) | None => {}
                }
            }
        }
        if !found.auto_follow {
            break;
        }
    }
    if preview && !state.can_select_preview {
        Some(Msg::CueReasonChainPreview.en())
    } else if audio && !state.can_control_audio {
        Some(Msg::CueReasonChainAudio.en())
    } else if title && !state.can_edit_project {
        Some(Msg::CueReasonChainTitle.en())
    } else {
        None
    }
}

/// The GO key's intent for `list`.
///
/// # Errors
///
/// Returns the session `block`, or why nothing can fire (no cue on
/// standby).
pub fn go_intent(
    list: &CueListStatus,
    block: Option<&'static str>,
) -> Result<CueIntent, &'static str> {
    if let Some(reason) = block {
        return Err(reason);
    }
    if list.state.standby.is_none() {
        return Err(Msg::CueReasonNoStandby.en());
    }
    Ok(CueIntent::Control {
        list: list.id,
        operation: CueOperation::Go { cue: None },
    })
}

/// The list the `G` key fires, if it may fire `list`: like the GO key, it
/// needs a cue on standby and the permissions of the chain that cue runs
/// ([`chain_block`]), including every verb of a script step's script.
/// (The `transition` gate is the caller's, as for every switcher key.)
#[must_use]
pub fn go_key_list(
    state: &StudioUiState,
    scripts: &[ScriptStatus],
    list: &CueListStatus,
) -> Option<u64> {
    let standby = list.state.standby?;
    chain_block(state, scripts, list, standby)
        .is_none()
        .then_some(list.id)
}

/// The milliseconds an operator's arm delay names: whole seconds as
/// `SS`, `MM:SS`, or `HH:MM:SS` (minutes and seconds below 60 after the
/// first part), from one second to [`MAX_CUE_ARM_MS`]. `None` otherwise.
#[must_use]
pub fn parse_arm_delay(text: &str) -> Option<u32> {
    let parts = text
        .trim()
        .split(':')
        .map(|part| {
            (!part.is_empty() && part.len() <= 6 && part.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| part.parse::<u64>().ok())
                .flatten()
        })
        .collect::<Option<Vec<_>>>()?;
    let seconds = match parts[..] {
        [seconds] => seconds,
        [minutes, seconds] if seconds < 60 => minutes * 60 + seconds,
        [hours, minutes, seconds] if minutes < 60 && seconds < 60 => {
            hours * 3_600 + minutes * 60 + seconds
        }
        _ => return None,
    };
    let millis = u32::try_from(seconds.checked_mul(1_000)?).ok()?;
    (1_000..=MAX_CUE_ARM_MS).contains(&millis).then_some(millis)
}

/// Why cue `cue` of `list` cannot be deleted from the list, if it cannot:
/// the list edit block, then a GO armed for it (the engine refuses to
/// remove an armed cue).
#[must_use]
pub fn cue_remove_block(
    list: &CueListStatus,
    cue: u64,
    edit: Option<&'static str>,
) -> Option<&'static str> {
    edit.or_else(|| {
        list.state
            .armed
            .is_some_and(|armed| armed.cue == cue)
            .then_some(Msg::CueReasonArmed.en())
    })
}

/// The lowest list id not in use, or `None` at the list limit.
#[must_use]
pub fn new_list_id(lists: &[CueListStatus]) -> Option<u64> {
    if lists.len() >= MAX_CUE_LISTS {
        return None;
    }
    (1..=MAX_CUE_LISTS as u64 + 1).find(|id| lists.iter().all(|list| list.id != *id))
}

/// One more than the highest cue id in `cues` (cue ids only need to be
/// unique within their list).
#[must_use]
pub fn new_cue_id(cues: &[WireCue]) -> u64 {
    cues.iter().map(|cue| cue.id).max().map_or(1, |id| id + 1)
}

/// A cue's run state, for its chip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CueRole {
    Idle,
    Live,
    Standby,
    Waiting,
    /// A GO of the cue is armed (Protocol 2.64).
    Armed,
    Played,
    Fault,
}

impl CueRole {
    /// The role of `cue` under `state`: a fault or wait outranks live, which
    /// outranks an armed GO, which outranks standby and played.
    #[must_use]
    pub fn of(state: &CueRunStatus, cue: u64) -> Self {
        if state.fault.as_ref().is_some_and(|fault| fault.cue == cue) {
            Self::Fault
        } else if state.pending.is_some_and(|pending| pending.cue == cue) {
            Self::Waiting
        } else if state.live == Some(cue) {
            Self::Live
        } else if state.armed.is_some_and(|armed| armed.cue == cue) {
            Self::Armed
        } else if state.standby == Some(cue) {
            Self::Standby
        } else if state.played.contains(&cue) {
            Self::Played
        } else {
            Self::Idle
        }
    }

    const fn chip(self) -> (Msg, Tone) {
        match self {
            Self::Idle => (Msg::CueStateNotPlayed, Tone::Idle),
            Self::Live => (Msg::Live, Tone::OnAir),
            Self::Standby => (Msg::CueStateStandby, Tone::Ready),
            Self::Waiting => (Msg::MediaStateWaiting, Tone::Pending),
            Self::Armed => (Msg::PlaylistStateArmed, Tone::Pending),
            Self::Played => (Msg::CueStatePlayed, Tone::Idle),
            Self::Fault => (Msg::CueStateFault, Tone::Fault),
        }
    }
}

/// What an action does: the kind picker's choices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionKind {
    Preview,
    Take,
    OverlayOn,
    OverlayOff,
    TitleOn,
    TitleText,
    TitleOff,
    FadeToBlack,
    PlaylistStart,
    PlaylistStop,
    AudioGain,
    AudioMute,
    Record,
    Stream,
    MediaPlay,
    MediaPause,
    MediaSeek,
    MediaRestart,
    Wait,
    Delay,
    RunScript,
}

impl ActionKind {
    pub const ALL: [Self; 21] = [
        Self::Preview,
        Self::Take,
        Self::OverlayOn,
        Self::OverlayOff,
        Self::TitleOn,
        Self::TitleText,
        Self::TitleOff,
        Self::FadeToBlack,
        Self::PlaylistStart,
        Self::PlaylistStop,
        Self::AudioGain,
        Self::AudioMute,
        Self::Record,
        Self::Stream,
        Self::MediaPlay,
        Self::MediaPause,
        Self::MediaSeek,
        Self::MediaRestart,
        Self::Wait,
        Self::Delay,
        Self::RunScript,
    ];

    #[must_use]
    pub const fn of(action: &WireCueAction) -> Self {
        match action {
            WireCueAction::Preview { .. } => Self::Preview,
            WireCueAction::Take { .. } => Self::Take,
            WireCueAction::OverlayOn { .. } => Self::OverlayOn,
            WireCueAction::OverlayOff { .. } => Self::OverlayOff,
            WireCueAction::TitleOn { .. } => Self::TitleOn,
            WireCueAction::TitleText { .. } => Self::TitleText,
            WireCueAction::TitleOff { .. } => Self::TitleOff,
            WireCueAction::FadeToBlack { .. } => Self::FadeToBlack,
            WireCueAction::PlaylistStart => Self::PlaylistStart,
            WireCueAction::PlaylistStop => Self::PlaylistStop,
            WireCueAction::AudioGain { .. } => Self::AudioGain,
            WireCueAction::AudioMute { .. } => Self::AudioMute,
            WireCueAction::Record { .. } => Self::Record,
            WireCueAction::Stream { .. } => Self::Stream,
            WireCueAction::Media { operation, .. } => match operation {
                WireCueMediaOperation::Play => Self::MediaPlay,
                WireCueMediaOperation::Pause => Self::MediaPause,
                WireCueMediaOperation::Seek { .. } => Self::MediaSeek,
                WireCueMediaOperation::Restart => Self::MediaRestart,
            },
            WireCueAction::Wait { .. } => Self::Wait,
            WireCueAction::Delay { .. } => Self::Delay,
            WireCueAction::RunScript { .. } => Self::RunScript,
        }
    }

    /// The kind's catalog label (English; drawing translates it).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Preview => Msg::PanelPreview.en(),
            Self::Take => Msg::TitleOpTake.en(),
            Self::OverlayOn => Msg::CueActOverlayOn.en(),
            Self::OverlayOff => Msg::CueActOverlayOff.en(),
            Self::TitleOn => Msg::CueActTitleOn.en(),
            Self::TitleText => Msg::CueActTitleText.en(),
            Self::TitleOff => Msg::CueActTitleOff.en(),
            Self::FadeToBlack => Msg::FadeToBlack.en(),
            Self::PlaylistStart => Msg::CueActPlaylistStart.en(),
            Self::PlaylistStop => Msg::CueActPlaylistStop.en(),
            Self::AudioGain => Msg::CueActGain.en(),
            Self::AudioMute => Msg::CueActMute.en(),
            Self::Record => Msg::CueActRecord.en(),
            Self::Stream => Msg::CueActStream.en(),
            Self::MediaPlay => Msg::CueActMediaPlay.en(),
            Self::MediaPause => Msg::CueActMediaPause.en(),
            Self::MediaSeek => Msg::CueActMediaSeek.en(),
            Self::MediaRestart => Msg::CueActMediaRestart.en(),
            Self::Wait => Msg::CueActWait.en(),
            Self::Delay => Msg::CueActDelay.en(),
            Self::RunScript => Msg::CueActRunScript.en(),
        }
    }

    /// A fresh action of this kind naming the show's first input, title,
    /// field, stream target, or script, or `None` when the show has none to
    /// name.
    #[must_use]
    pub fn default_action(
        self,
        view: &ClientView,
        project: &ProjectState,
    ) -> Option<WireCueAction> {
        let input = || view.inputs.first().copied().map(WireInputId::from_domain);
        let title = || project.titles().first();
        let media = || {
            media_inputs(view, project)
                .first()
                .copied()
                .map(WireInputId::from_domain)
        };
        let transport = |operation| {
            Some(WireCueAction::Media {
                input: media()?,
                operation,
            })
        };
        let channel = WireOverlayChannelId::new(1)?;
        Some(match self {
            Self::Preview => WireCueAction::Preview { input: input()? },
            Self::Take => WireCueAction::Take {
                input: None,
                effect: TransitionFavoriteEffect::Cut,
                duration_frames: 0,
            },
            Self::OverlayOn => WireCueAction::OverlayOn {
                channel,
                input: input()?,
            },
            Self::OverlayOff => WireCueAction::OverlayOff { channel },
            Self::TitleOn => WireCueAction::TitleOn {
                input: WireInputId::from_domain(title()?.input),
                channel,
            },
            Self::TitleText => {
                let title = title()?;
                WireCueAction::TitleText {
                    input: WireInputId::from_domain(title.input),
                    field: title.fields.first()?.field,
                    text: String::new(),
                }
            }
            Self::TitleOff => WireCueAction::TitleOff {
                input: WireInputId::from_domain(title()?.input),
            },
            Self::FadeToBlack => WireCueAction::FadeToBlack {
                active: true,
                duration_frames: 25,
            },
            Self::PlaylistStart => WireCueAction::PlaylistStart,
            Self::PlaylistStop => WireCueAction::PlaylistStop,
            Self::AudioGain => WireCueAction::AudioGain {
                input: input()?,
                gain_millidb: 0,
            },
            Self::AudioMute => WireCueAction::AudioMute {
                input: input()?,
                muted: true,
            },
            Self::Record => WireCueAction::Record {
                recorder: WireRecorderSlot::new(1)?,
                start: true,
            },
            Self::Stream => WireCueAction::Stream {
                target: WireStreamTargetId::new(view.streams.first()?.target),
                start: true,
            },
            Self::MediaPlay => transport(WireCueMediaOperation::Play)?,
            Self::MediaPause => transport(WireCueMediaOperation::Pause)?,
            Self::MediaSeek => transport(WireCueMediaOperation::Seek { frame: 0 })?,
            Self::MediaRestart => transport(WireCueMediaOperation::Restart)?,
            Self::Wait => WireCueAction::Wait { frames: 25 },
            Self::Delay => WireCueAction::Delay { millis: 1_000 },
            Self::RunScript => WireCueAction::RunScript {
                script: project.scripts().first()?.id,
            },
        })
    }
}

/// The show's media inputs, in input order: the media actions' choices.
fn media_inputs(view: &ClientView, project: &ProjectState) -> Vec<InputId> {
    view.inputs
        .iter()
        .copied()
        .filter(|input| project.media_status(*input).is_some())
        .collect()
}

/// The working copy of one list: `seen` is the replicated definition last
/// observed, `sent` the definitions this console sent since (oldest first)
/// whose echo has not landed, and `spec` the operator's edits.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Draft {
    list: u64,
    seen: CueListSpec,
    sent: Vec<CueListSpec>,
    spec: CueListSpec,
}

impl Draft {
    fn of(list: &CueListStatus) -> Self {
        Self {
            list: list.id,
            seen: list.spec.clone(),
            sent: Vec::new(),
            spec: list.spec.clone(),
        }
    }
}

/// The Cues tab's local state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CuePanel {
    selected: Option<u64>,
    /// The cue whose actions are shown.
    expanded: Option<u64>,
    draft: Option<Draft>,
    /// The arm delay as typed (Protocol 2.64).
    arm_delay: String,
    /// The newest cue frame seen and when it was first seen, for the armed
    /// GO's countdown.
    clock: Option<(u64, Instant)>,
}

/// The arm delay a fresh panel offers: one minute.
const DEFAULT_ARM_DELAY: &str = "1:00";

/// One edit the rows asked for this frame.
enum Change {
    /// Send the working copy now.
    Commit,
    /// Leave it drafted (a text or number still being edited).
    Draft,
}

impl CuePanel {
    /// The list the tab shows: the selected one while it exists, else the
    /// first.
    fn shown<'a>(&self, project: &'a ProjectState) -> Option<&'a CueListStatus> {
        let lists = project.cue_lists();
        self.selected
            .and_then(|id| lists.iter().find(|list| list.id == id))
            .or_else(|| lists.first())
    }

    /// The list `G` fires: the shown list while it has a cue on standby
    /// whose chain the session may fire ([`go_key_list`]).
    pub(crate) fn go_list(&self, state: &StudioUiState) -> Option<u64> {
        let project = state.project_state.as_ref()?;
        go_key_list(state, project.scripts(), self.shown(project)?)
    }

    /// Shows list `list` (for tests and fixtures).
    pub(crate) fn select(&mut self, list: u64, expanded: Option<u64>) {
        self.selected = Some(list);
        self.expanded = expanded;
    }

    /// The arm row (Protocol 2.64): the delay field, Arm GO (gated like GO
    /// by `go_block`), and Disarm (gated by `control`, and on an armed GO).
    fn draw_arm(
        &mut self,
        ui: &mut Ui,
        list: &CueListStatus,
        go_block: Option<&'static str>,
        control: Option<&'static str>,
        intents: &mut Vec<StudioIntent>,
    ) {
        let l = l10n::locale(ui.ctx());
        if self.arm_delay.is_empty() {
            DEFAULT_ARM_DELAY.clone_into(&mut self.arm_delay);
        }
        let delay = parse_arm_delay(&self.arm_delay);
        ui.horizontal(|ui| {
            ui.label(field_label(l.text(Msg::CueArmIn)));
            ui.add(
                TextEdit::singleline(&mut self.arm_delay)
                    .desired_width(72.0)
                    .font(egui::TextStyle::Monospace),
            )
            .on_hover_text(l.text(Msg::CueArmDelayHover));
            let arm = go_block.or_else(|| delay.is_none().then_some(Msg::CueReasonArmDelay.en()));
            if ui
                .add_enabled(arm.is_none(), key_button(l.text(Msg::CueArm), None, 72.0))
                .hint(l.text(Msg::CueArmHover), arm.unwrap_or(""))
                .clicked()
                && let Some(in_ms) = delay
            {
                intents.push(StudioIntent::Cue(CueIntent::Control {
                    list: list.id,
                    operation: CueOperation::Arm { cue: None, in_ms },
                }));
            }
            let disarm = control.or_else(|| {
                list.state
                    .armed
                    .is_none()
                    .then_some(Msg::CueReasonNotArmed.en())
            });
            if ui
                .add_enabled(
                    disarm.is_none(),
                    key_button(l.text(Msg::CueDisarm), None, 64.0),
                )
                .hint(l.text(Msg::CueDisarmHover), disarm.unwrap_or(""))
                .clicked()
            {
                intents.push(StudioIntent::Cue(CueIntent::Control {
                    list: list.id,
                    operation: CueOperation::Disarm,
                }));
            }
        });
    }

    /// Keeps the working copy in step with the replicated list. The echo of
    /// an edit this console sent keeps the copy (and any later edits in it);
    /// any other change to the replicated list, or a sent edit that resolved
    /// without an echo (`pending` is whether this session still awaits
    /// command results), takes a fresh copy, so a refused edit snaps back.
    fn sync_draft(&mut self, list: &CueListStatus, pending: bool) {
        let Some(draft) = self.draft.as_mut().filter(|draft| draft.list == list.id) else {
            self.draft = Some(Draft::of(list));
            return;
        };
        if draft.seen != list.spec {
            draft.seen = list.spec.clone();
            match draft.sent.iter().position(|sent| *sent == list.spec) {
                Some(echo) => {
                    draft.sent.drain(..=echo);
                }
                None => *draft = Draft::of(list),
            }
        } else if !draft.sent.is_empty() && !pending {
            *draft = Draft::of(list);
        }
    }

    /// The edit that sends the working copy, unless it holds nothing new
    /// or would take Program twice on one frame (see [`take_conflicts`];
    /// the rows say why and the copy stays drafted until it is fixed).
    fn commit_draft(&mut self) -> Option<CueIntent> {
        let draft = self.draft.as_mut()?;
        if draft.spec == *draft.sent.last().unwrap_or(&draft.seen)
            || !take_conflicts(&draft.spec.cues).is_empty()
        {
            return None;
        }
        draft.sent.push(draft.spec.clone());
        Some(CueIntent::Set {
            list: draft.list,
            spec: draft.spec.clone(),
        })
    }

    #[allow(clippy::too_many_lines)] // one immediate-mode tab
    pub(crate) fn draw(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let l = l10n::locale(ui.ctx());
        let (Some(view), Some(project)) = (state.view.as_ref(), state.project_state.as_ref())
        else {
            empty_state(ui, l.text(Msg::TabCues), l.text(Msg::PlaylistNotSynced));
            return;
        };
        let control = control_block(state);
        let edit = edit_block(state);
        let lists = project.cue_lists();
        let shown = self.shown(project).cloned();

        ui.horizontal(|ui| {
            if !lists.is_empty() {
                let current = shown
                    .as_ref()
                    .map(|list| list.spec.name.clone())
                    .unwrap_or_default();
                ComboBox::from_id_salt("studio-cue-list-pick")
                    .width(140.0)
                    .truncate()
                    .selected_text(RichText::new(current).size(legible(11.0)))
                    .show_ui(ui, |ui| {
                        for list in lists {
                            let selected = shown.as_ref().is_some_and(|shown| shown.id == list.id);
                            if ui
                                .selectable_label(selected, &list.spec.name)
                                .on_hover_text(l.text(Msg::CueListPickHover))
                                .clicked()
                            {
                                self.selected = Some(list.id);
                                self.expanded = None;
                            }
                        }
                    })
                    .response
                    .on_hover_text(l.text(Msg::CueListPickHover));
            }
            let new_id = new_list_id(lists);
            let reason = edit.unwrap_or(Msg::CueReasonListsFull.en());
            if ui
                .add_enabled(
                    edit.is_none() && new_id.is_some(),
                    small_button(l.text(Msg::CueListNew)),
                )
                .hint(l.text(Msg::CueListNewHover), reason)
                .clicked()
                && let Some(id) = new_id
            {
                let number = id.to_string();
                intents.push(StudioIntent::Cue(CueIntent::Set {
                    list: id,
                    spec: CueListSpec {
                        name: l.format(Msg::CueListDefaultName, &[("number", Arg::Str(&number))]),
                        cues: Vec::new(),
                    },
                }));
                self.selected = Some(id);
            }
            let remove = edit.or_else(|| {
                shown
                    .as_ref()
                    .is_some_and(|list| list.state.armed.is_some())
                    .then_some(Msg::CueReasonArmed.en())
            });
            if let Some(list) = &shown
                && ui
                    .add_enabled(remove.is_none(), small_button(l.text(Msg::CueListRemove)))
                    .hint(l.text(Msg::CueListRemoveHover), remove.unwrap_or(""))
                    .clicked()
            {
                intents.push(StudioIntent::Cue(CueIntent::Remove { list: list.id }));
            }
        });
        let Some(list) = shown else {
            empty_state(ui, l.text(Msg::TabCues), l.text(Msg::CueListsEmpty));
            return;
        };
        self.sync_draft(&list, state.pending_commands > 0);
        let list_edit = list_edit_block(state, &list);
        let now = Instant::now();
        if let Some(frame) = project.cue_frame()
            && self.clock.is_none_or(|(seen, _)| seen != frame)
        {
            self.clock = Some((frame, now));
        }
        ui.add_space(4.0);

        // The GO key and the cue on standby.
        let go = go_intent(
            &list,
            control.or_else(|| {
                list.state
                    .standby
                    .and_then(|standby| chain_block(state, project.scripts(), &list, standby))
            }),
        );
        let standby_name = list
            .state
            .standby
            .and_then(|id| list.spec.cues.iter().find(|cue| cue.id == id))
            .map(|cue| cue.name.clone());
        // Growing atoms on both sides centre the label in the full-width key.
        let go_button = Button::new((
            egui::Atom::grow(),
            RichText::new(l.text(Msg::CueGo))
                .size(legible(22.0))
                .strong(),
            egui::Atom::grow(),
        ))
        .fill(PREVIEW.gamma_multiply(if go.is_ok() { 0.45 } else { 0.12 }))
        .stroke(Stroke::new(2.0, PREVIEW.gamma_multiply(0.8)))
        .min_size(Vec2::new(ui.available_width(), 48.0));
        if ui
            .add_enabled(go.is_ok(), go_button)
            .hint(
                l.text(Msg::CueGoHover),
                go.as_ref().err().copied().unwrap_or(""),
            )
            .clicked()
            && let Ok(intent) = go.clone()
        {
            intents.push(StudioIntent::Cue(intent));
        }
        ui.label(
            RichText::new(standby_name.map_or_else(
                || l.text(Msg::CueGoNothing).to_owned(),
                |name| l.format(Msg::CueStandbyLabel, &[("name", Arg::Str(&name))]),
            ))
            .size(legible(12.0))
            .strong()
            .color(PREVIEW),
        );
        ui.horizontal(|ui| {
            let operations = [
                (Msg::CueBack, Msg::CueBackHover, CueOperation::Back),
                (Msg::CueSkip, Msg::CueSkipHover, CueOperation::Skip),
                (Msg::PlaylistStop, Msg::CueStopHover, CueOperation::Stop),
                (Msg::CueReset, Msg::CueResetHover, CueOperation::Reset),
            ];
            for (label, hover, operation) in operations {
                if ui
                    .add_enabled(control.is_none(), key_button(l.text(label), None, 64.0))
                    .hint(l.text(hover), control.unwrap_or(""))
                    .clicked()
                {
                    intents.push(StudioIntent::Cue(CueIntent::Control {
                        list: list.id,
                        operation,
                    }));
                }
            }
        });
        self.draw_arm(ui, &list, go.as_ref().err().copied(), control, intents);
        if let Some(armed) = list.state.armed {
            let name = list
                .spec
                .cues
                .iter()
                .find(|cue| cue.id == armed.cue)
                .map_or_else(|| armed.cue.to_string(), |cue| cue.name.clone());
            let frame = armed.due_frame.to_string();
            let rate = project.frame_rate();
            let note = match self.clock {
                Some((seen, at)) => {
                    let now_frame = estimated_frame(seen, now.saturating_duration_since(at), rate);
                    let time = countdown_timecode(armed.due_frame.saturating_sub(now_frame), rate);
                    l.format(
                        Msg::CueArmedNote,
                        &[
                            ("name", Arg::Str(&name)),
                            ("time", Arg::Str(&time)),
                            ("frame", Arg::Str(&frame)),
                        ],
                    )
                }
                None => l.format(
                    Msg::CueArmedFrameNote,
                    &[("name", Arg::Str(&name)), ("frame", Arg::Str(&frame))],
                ),
            };
            ui.label(RichText::new(note).size(legible(12.0)).strong());
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        if let Some(pending) = list.state.pending {
            let frame = pending.due_frame.to_string();
            ui.label(caption(
                l.format(Msg::CueWaitingNote, &[("frame", Arg::Str(&frame))]),
            ));
        }
        if let Some(fault) = &list.state.fault {
            ui.label(RichText::new(fault_note(l, state, fault)).color(ERROR));
        }
        if !state.can_transition {
            ui.label(caption(l.text(Msg::CueViewOnly)));
        }
        ui.add_space(6.0);

        let Some(draft) = self.draft.as_mut() else {
            return;
        };
        let mut change = None;
        let name = ui
            .add_enabled(
                list_edit.is_none(),
                TextEdit::singleline(&mut draft.spec.name).desired_width(ui.available_width()),
            )
            .hint(l.text(Msg::CueListNameHover), list_edit.unwrap_or(""));
        if name.lost_focus() {
            change = Some(Change::Commit);
        }
        ui.add_space(4.0);
        if draft.spec.cues.is_empty() {
            ui.label(caption(l.text(Msg::CueListEmpty)));
        }
        let script_steps = draft
            .spec
            .cues
            .iter()
            .flat_map(|cue| &cue.actions)
            .filter(|action| matches!(action, WireCueAction::RunScript { .. }))
            .count();
        let names = Names {
            l,
            view,
            project,
            scripts_full: script_steps >= MAX_CUE_LIST_SCRIPT_STEPS,
        };
        let mut edit_row = None;
        let conflicts = take_conflicts(&draft.spec.cues);
        for index in 0..draft.spec.cues.len() {
            // A row removed this frame shortens the list.
            let Some(id) = draft.spec.cues.get(index).map(|cue| cue.id) else {
                break;
            };
            let conflict = conflicts
                .iter()
                .find(|(cue, _)| *cue == id)
                .and_then(|(_, earlier)| {
                    draft
                        .spec
                        .cues
                        .iter()
                        .find(|cue| cue.id == *earlier)
                        .map(|cue| (cue.id, cue.name.clone()))
                });
            // Firing runs the saved cue and every cue it follows into.
            let fire = control.or_else(|| chain_block(state, project.scripts(), &list, id));
            let remove = cue_remove_block(&list, id, list_edit);
            let row = draw_cue_row(
                ui,
                &names,
                &list,
                (&mut draft.spec.cues, index),
                (control, fire, list_edit, remove),
                (self.expanded, conflict),
                intents,
            );
            if let Some(row) = row {
                edit_row = Some(row);
            }
        }
        match edit_row {
            Some(RowEdit::Expand(cue)) => {
                self.expanded = (self.expanded != Some(cue)).then_some(cue);
            }
            Some(RowEdit::Change(row_change)) => change = Some(row_change),
            None => {}
        }
        let full = draft.spec.cues.len() >= MAX_CUES;
        let reason = list_edit.unwrap_or(Msg::CueReasonCuesFull.en());
        if ui
            .add_enabled(
                list_edit.is_none() && !full,
                small_button(l.text(Msg::CueAdd)),
            )
            .hint(l.text(Msg::CueAddHover), reason)
            .clicked()
        {
            let id = new_cue_id(&draft.spec.cues);
            let number = id.to_string();
            draft.spec.cues.push(WireCue {
                id,
                name: l.format(Msg::CueDefaultName, &[("number", Arg::Str(&number))]),
                auto_follow: false,
                actions: Vec::new(),
            });
            self.expanded = Some(id);
            change = Some(Change::Commit);
        }
        if matches!(change, Some(Change::Commit))
            && let Some(intent) = self.commit_draft()
        {
            intents.push(StudioIntent::Cue(intent));
        }
    }
}

/// Names in the console language.
struct Names<'a> {
    l: Locale,
    view: &'a ClientView,
    project: &'a ProjectState,
    /// Whether the list already runs its maximum of scripts.
    scripts_full: bool,
}

impl Names<'_> {
    fn input(&self, input: WireInputId) -> String {
        input_name(
            self.l,
            &self.view.inputs,
            &self.view.input_names,
            input.to_domain(),
        )
    }
}

enum RowEdit {
    Expand(u64),
    Change(Change),
}

/// Width of a cue row's run-state chip.
const CHIP_WIDTH: f32 = 64.0;
/// Width of a cue row's standby, fire, move, and remove keys.
const ROW_KEY_WIDTH: f32 = 28.0;
/// Width of a cue row's action-count key.
const COUNT_KEY_WIDTH: f32 = 36.0;
/// The narrowest name field a one-line cue row keeps; narrower docks move
/// the follow box and keys to a second line.
const MIN_ONE_LINE_NAME: f32 = 110.0;

/// Width of the follow box and the five row keys, with their spacing.
fn row_controls_width(ui: &Ui, follow: &str) -> f32 {
    let spacing = ui.spacing();
    let font = egui::TextStyle::Button.resolve(ui.style());
    let text = ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(follow.to_owned(), font, egui::Color32::WHITE)
            .size()
            .x
    });
    let follow_width = spacing.icon_width + spacing.icon_spacing + text;
    follow_width + 5.0 * ROW_KEY_WIDTH + 5.0 * spacing.item_spacing.x + 2.0
}

/// A cue's name field, `width` wide. While it is not being edited a name
/// that does not fit ends in `…`, and the hover repeats it in full.
fn cue_name_field(
    ui: &mut Ui,
    l: Locale,
    name: &mut String,
    width: f32,
    edit: Option<&'static str>,
) -> egui::Response {
    let id = ui.id().with("name");
    let focused = ui.memory(|memory| memory.has_focus(id));
    let font = egui::TextStyle::Body.resolve(ui.style());
    let color = ui
        .visuals()
        .override_text_color
        .unwrap_or_else(|| ui.visuals().widgets.inactive.text_color());
    let mut elided = false;
    let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
        let mut job =
            egui::text::LayoutJob::simple_singleline(text.as_str().to_owned(), font.clone(), color);
        if !focused {
            job.wrap = egui::text::TextWrapping::truncate_at_width(wrap_width.max(1.0));
        }
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
        elided |= galley.elided;
        galley
    };
    let response = ui.add_enabled(
        edit.is_none(),
        TextEdit::singleline(name)
            .id(id)
            .desired_width(width)
            .layouter(&mut layouter),
    );
    let hover = l.text(Msg::CueNameHover);
    if elided {
        let full = format!("{name}\n{hover}");
        response.hint(&full, edit.unwrap_or(""))
    } else {
        response.hint(hover, edit.unwrap_or(""))
    }
}

/// One cue: its chip, name, follow box, standby, fire, move, and remove
/// keys, the action-count key, and (expanded) its actions. The name takes
/// the width the keys leave; in a narrow dock the follow box and keys move
/// to a second line under the chip, name, and count key.
fn draw_cue_row(
    ui: &mut Ui,
    names: &Names<'_>,
    list: &CueListStatus,
    (cues, index): (&mut Vec<WireCue>, usize),
    (control, fire, edit, remove): (
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
    ),
    (expanded, conflict): (Option<u64>, Option<(u64, String)>),
    intents: &mut Vec<StudioIntent>,
) -> Option<RowEdit> {
    let l = names.l;
    let id = cues[index].id;
    let role = CueRole::of(&list.state, id);
    let mut result = None;
    let stroke = match role {
        CueRole::Live | CueRole::Waiting | CueRole::Armed => Stroke::new(1.5, PROGRAM),
        CueRole::Standby => Stroke::new(1.5, PREVIEW),
        _ => Stroke::new(1.0, border_color()),
    };
    Frame::new()
        .fill(raised_color())
        .stroke(stroke)
        .corner_radius(4.0)
        .inner_margin(4.0)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.push_id(("studio-cue", id), |ui| {
                let open = expanded == Some(id);
                let spacing = ui.spacing().item_spacing.x;
                let controls = row_controls_width(ui, l.text(Msg::CueFollow));
                let head = CHIP_WIDTH + COUNT_KEY_WIDTH + 2.0 * spacing;
                let two_line_name = ui.available_width() - head - spacing;
                let one_line = two_line_name - controls >= MIN_ONE_LINE_NAME;
                let name_width = if one_line {
                    two_line_name - controls
                } else {
                    two_line_name
                };
                let mut removed = false;
                ui.horizontal(|ui| {
                    let (msg, tone) = role.chip();
                    status_chip(ui, l.text(msg), tone, CHIP_WIDTH)
                        .on_hover_text(l.text(Msg::CueStateHover));
                    let name =
                        cue_name_field(ui, l, &mut cues[index].name, name_width.max(40.0), edit);
                    if name.lost_focus() {
                        result = Some(RowEdit::Change(Change::Commit));
                    }
                    if !one_line {
                        if count_key(ui, l, cues[index].actions.len(), open) {
                            result = Some(RowEdit::Expand(id));
                        }
                        return;
                    }
                    if let Some(change) = row_controls(
                        ui,
                        l,
                        list.id,
                        (cues, index),
                        (control, fire, edit, remove),
                        intents,
                    ) {
                        removed = matches!(change, RowControl::Removed);
                        result = Some(RowEdit::Change(Change::Commit));
                    }
                    if !removed && count_key(ui, l, cues[index].actions.len(), open) {
                        result = Some(RowEdit::Expand(id));
                    }
                });
                if !one_line {
                    ui.horizontal(|ui| {
                        ui.add_space(CHIP_WIDTH);
                        if let Some(change) = row_controls(
                            ui,
                            l,
                            list.id,
                            (cues, index),
                            (control, fire, edit, remove),
                            intents,
                        ) {
                            removed = matches!(change, RowControl::Removed);
                            result = Some(RowEdit::Change(Change::Commit));
                        }
                    });
                }
                if removed {
                    return;
                }
                if let Some((earlier, name)) = &conflict {
                    let note = if *earlier == id {
                        l.text(Msg::CueTakeTwiceNote).to_owned()
                    } else {
                        l.format(Msg::CueTakeFollowNote, &[("name", Arg::Str(name))])
                    };
                    ui.label(RichText::new(note).size(legible(11.0)).color(ERROR));
                }
                if open
                    && index < cues.len()
                    && let Some(change) = draw_actions(ui, names, &mut cues[index], edit)
                {
                    result = Some(RowEdit::Change(change));
                }
            });
        });
    ui.add_space(2.0);
    result
}

/// The action-count key, which opens and closes a cue's actions.
fn count_key(ui: &mut Ui, l: Locale, count: usize, open: bool) -> bool {
    let label = format!("{count} {}", if open { "−" } else { "+" });
    ui.add(key_button(&label, None, COUNT_KEY_WIDTH))
        .on_hover_text(l.text(Msg::CueExpandHover))
        .clicked()
}

/// What a row's follow box and keys did to the working copy.
enum RowControl {
    Changed,
    Removed,
}

/// A cue row's follow box and its standby, fire, move, and remove keys.
fn row_controls(
    ui: &mut Ui,
    l: Locale,
    list: u64,
    (cues, index): (&mut Vec<WireCue>, usize),
    (control, fire, edit, remove): (
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
    ),
    intents: &mut Vec<StudioIntent>,
) -> Option<RowControl> {
    let id = cues[index].id;
    let mut result = None;
    let follow = ui
        .add_enabled(
            edit.is_none(),
            egui::Checkbox::new(&mut cues[index].auto_follow, l.text(Msg::CueFollow)),
        )
        .hint(l.text(Msg::CueFollowHover), edit.unwrap_or(""));
    if follow.changed() {
        result = Some(RowControl::Changed);
    }
    if add_icon_button(
        ui,
        control.is_none(),
        key_button("", None, ROW_KEY_WIDTH),
        KeyIcon::Dot,
        l.text(Msg::CueJumpHover),
    )
    .hint(l.text(Msg::CueJumpHover), control.unwrap_or(""))
    .clicked()
    {
        intents.push(StudioIntent::Cue(CueIntent::Control {
            list,
            operation: CueOperation::Standby { cue: id },
        }));
    }
    if ui
        .add_enabled(fire.is_none(), key_button("▶", None, ROW_KEY_WIDTH))
        .hint(l.text(Msg::CueFireHover), fire.unwrap_or(""))
        .clicked()
    {
        intents.push(StudioIntent::Cue(CueIntent::Control {
            list,
            operation: CueOperation::Go { cue: Some(id) },
        }));
    }
    let moves = [
        (
            KeyIcon::Up,
            Msg::CueUpHover,
            index > 0,
            index.wrapping_sub(1),
            Msg::CueReasonFirst,
        ),
        (
            KeyIcon::Down,
            Msg::CueDownHover,
            index + 1 < cues.len(),
            index + 1,
            Msg::CueReasonLast,
        ),
    ];
    for (icon, hover, possible, other, end) in moves {
        let reason = edit.unwrap_or(end.en());
        if add_icon_button(
            ui,
            edit.is_none() && possible,
            key_button("", None, ROW_KEY_WIDTH),
            icon,
            l.text(hover),
        )
        .hint(l.text(hover), reason)
        .clicked()
        {
            cues.swap(index, other);
            result = Some(RowControl::Changed);
        }
    }
    if add_icon_button(
        ui,
        remove.is_none(),
        key_button("", Some(ERROR), ROW_KEY_WIDTH),
        KeyIcon::Remove,
        l.text(Msg::CueRemoveHover),
    )
    .hint(l.text(Msg::CueRemoveHover), remove.unwrap_or(""))
    .clicked()
    {
        cues.remove(index);
        result = Some(RowControl::Removed);
    }
    result
}

/// Width of an action row's move and remove keys.
const ACTION_KEY_WIDTH: f32 = 24.0;

/// A cue's actions, each with its kind picker, parameters, and move and
/// remove keys, then the Add action key.
#[allow(clippy::too_many_lines)]
fn draw_actions(
    ui: &mut Ui,
    names: &Names<'_>,
    cue: &mut WireCue,
    edit: Option<&'static str>,
) -> Option<Change> {
    let l = names.l;
    let mut change = None;
    let mut remove = None;
    let mut swap = None;
    let count = cue.actions.len();
    for index in 0..count {
        ui.push_id(("studio-cue-action", index), |ui| {
            // The move and remove keys keep a fixed column on the first
            // line; a narrow dock wraps the kind and parameters instead.
            ui.horizontal_top(|ui| {
                let keys = 3.0 * (ACTION_KEY_WIDTH + ui.spacing().item_spacing.x);
                let text = (ui.available_width() - keys).max(80.0);
                let layout = egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true);
                let height = ui.spacing().interact_size.y;
                ui.allocate_ui_with_layout(Vec2::new(text, height), layout, |ui| {
                    ui.set_min_width(text);
                    ui.label(field_label(&(index + 1).to_string()));
                    let current = ActionKind::of(&cue.actions[index]);
                    let width = fit_width(ui, 104.0);
                    ui.add_enabled_ui(edit.is_none(), |ui| {
                        ComboBox::from_id_salt("kind")
                            .width(width)
                            .truncate()
                            .selected_text(RichText::new(l.tr(current.label())).size(legible(11.0)))
                            .show_ui(ui, |ui| {
                                for kind in ActionKind::ALL {
                                    let full = kind == ActionKind::RunScript
                                        && current != ActionKind::RunScript
                                        && names.scripts_full;
                                    let fresh = (!full)
                                        .then(|| kind.default_action(names.view, names.project))
                                        .flatten();
                                    let reason = if full {
                                        Msg::CueReasonNoScriptsLeft
                                    } else {
                                        Msg::CueReasonNothingToName
                                    };
                                    let response = ui
                                        .add_enabled(
                                            fresh.is_some(),
                                            egui::Button::selectable(
                                                kind == current,
                                                l.tr(kind.label()),
                                            ),
                                        )
                                        .hint(
                                            l.text(Msg::CueActionKindHover),
                                            edit.unwrap_or(reason.en()),
                                        );
                                    if response.clicked()
                                        && kind != current
                                        && let Some(fresh) = fresh
                                    {
                                        cue.actions[index] = fresh;
                                        change = Some(Change::Commit);
                                    }
                                }
                            })
                            .response
                            .hint(l.text(Msg::CueActionKindHover), edit.unwrap_or(""));
                    });
                    if let Some(param) = draw_params(ui, names, &mut cue.actions[index], edit) {
                        change = Some(param);
                    }
                });
                if add_icon_button(
                    ui,
                    edit.is_none() && index > 0,
                    key_button("", None, ACTION_KEY_WIDTH),
                    KeyIcon::Up,
                    l.text(Msg::CueActionUpHover),
                )
                .hint(
                    l.text(Msg::CueActionUpHover),
                    edit.unwrap_or(Msg::CueReasonFirst.en()),
                )
                .clicked()
                {
                    swap = Some((index, index - 1));
                }
                if add_icon_button(
                    ui,
                    edit.is_none() && index + 1 < count,
                    key_button("", None, ACTION_KEY_WIDTH),
                    KeyIcon::Down,
                    l.text(Msg::CueActionDownHover),
                )
                .hint(
                    l.text(Msg::CueActionDownHover),
                    edit.unwrap_or(Msg::CueReasonLast.en()),
                )
                .clicked()
                {
                    swap = Some((index, index + 1));
                }
                if add_icon_button(
                    ui,
                    edit.is_none(),
                    key_button("", Some(ERROR), ACTION_KEY_WIDTH),
                    KeyIcon::Remove,
                    l.text(Msg::CueActionRemoveHover),
                )
                .hint(l.text(Msg::CueActionRemoveHover), edit.unwrap_or(""))
                .clicked()
                {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some((a, b)) = swap {
        cue.actions.swap(a, b);
        change = Some(Change::Commit);
    }
    if let Some(index) = remove {
        cue.actions.remove(index);
        change = Some(Change::Commit);
    }
    let full = cue.actions.len() >= MAX_CUE_ACTIONS;
    let reason = edit.unwrap_or(Msg::CueReasonActionsFull.en());
    if ui
        .add_enabled(
            edit.is_none() && !full,
            small_button(l.text(Msg::CueActionAdd)),
        )
        .hint(l.text(Msg::CueActionAddHover), reason)
        .clicked()
        && let Some(action) = ActionKind::Take.default_action(names.view, names.project)
    {
        cue.actions.push(action);
        change = Some(Change::Commit);
    }
    change
}

/// The narrowest an action's picker or text field shrinks before it wraps
/// to the next line.
const MIN_FIELD_WIDTH: f32 = 56.0;

/// The width for an action picker or field of `preferred` width in the
/// wrapping action row: what is left of the line, never more than
/// `preferred`. When less than [`MIN_FIELD_WIDTH`] is left the row wraps
/// first. Pickers sit in their own scope, which the wrapping layout cannot
/// move, so the row makes room for them here.
fn fit_width(ui: &mut Ui, preferred: f32) -> f32 {
    let padding = 2.0 * ui.spacing().button_padding.x;
    let room = |ui: &Ui| ui.available_size_before_wrap().x - padding;
    if room(ui) < MIN_FIELD_WIDTH && ui.cursor().left() > ui.max_rect().left() + 1.0 {
        ui.end_row();
    }
    preferred.min(room(ui)).max(MIN_FIELD_WIDTH)
}

/// An input picker over `choices`; `allow_preview` adds the Preview choice
/// (`None`).
fn input_picker(
    ui: &mut Ui,
    names: &Names<'_>,
    salt: &str,
    (value, choices): (&mut Option<WireInputId>, &[InputId]),
    allow_preview: bool,
    edit: Option<&'static str>,
) -> bool {
    let l = names.l;
    let hover = l.text(if salt == "media" {
        Msg::CueParamMediaHover
    } else {
        Msg::CueParamInputHover
    });
    let text = value.map_or_else(
        || l.text(Msg::CueParamPreview).to_owned(),
        |input| names.input(input),
    );
    let mut changed = false;
    let width = fit_width(ui, 110.0);
    ui.add_enabled_ui(edit.is_none(), |ui| {
        ComboBox::from_id_salt(salt)
            .width(width)
            .truncate()
            .selected_text(RichText::new(text).size(legible(11.0)))
            .show_ui(ui, |ui| {
                let options = allow_preview.then_some(None).into_iter().chain(
                    choices
                        .iter()
                        .map(|input| Some(WireInputId::from_domain(*input))),
                );
                for option in options {
                    let label = option.map_or_else(
                        || l.text(Msg::CueParamPreview).to_owned(),
                        |input| names.input(input),
                    );
                    if ui
                        .selectable_label(*value == option, label)
                        .on_hover_text(hover)
                        .clicked()
                        && *value != option
                    {
                        *value = option;
                        changed = true;
                    }
                }
            })
            .response
            .hint(hover, edit.unwrap_or(""));
    });
    changed
}

/// A numeric field; reports a commit when its drag ends or it loses focus.
fn number<T: egui::emath::Numeric>(
    ui: &mut Ui,
    value: &mut T,
    range: core::ops::RangeInclusive<T>,
    (hover, suffix): (&str, &str),
    (enabled, reason): (bool, &str),
) -> Option<Change> {
    let response = ui
        .add_enabled(enabled, DragValue::new(value).range(range).suffix(suffix))
        .hint(hover, reason);
    if response.drag_stopped() || response.lost_focus() {
        Some(Change::Commit)
    } else if response.changed() {
        if response.dragged() || response.has_focus() {
            Some(Change::Draft)
        } else {
            Some(Change::Commit)
        }
    } else {
        None
    }
}

/// An on/off box; any toggle commits.
fn flag(ui: &mut Ui, value: &mut bool, hover: &str, edit: Option<&'static str>) -> Option<Change> {
    let l = l10n::locale(ui.ctx());
    ui.add_enabled(
        edit.is_none(),
        egui::Checkbox::new(value, l.text(Msg::CueParamOn)),
    )
    .hint(hover, edit.unwrap_or(""))
    .changed()
    .then_some(Change::Commit)
}

fn channel(
    ui: &mut Ui,
    value: &mut WireOverlayChannelId,
    l: Locale,
    edit: Option<&'static str>,
) -> Option<Change> {
    let mut number_value = value.number();
    let change = number(
        ui,
        &mut number_value,
        1..=8,
        (l.text(Msg::CueParamChannelHover), ""),
        (edit.is_none(), edit.unwrap_or("")),
    );
    if let Some(channel) = WireOverlayChannelId::new(number_value) {
        *value = channel;
    }
    change
}

/// One action's parameters.
#[allow(clippy::too_many_lines)] // one picker per action kind
fn draw_params(
    ui: &mut Ui,
    names: &Names<'_>,
    action: &mut WireCueAction,
    edit: Option<&'static str>,
) -> Option<Change> {
    let l = names.l;
    let enabled = (edit.is_none(), edit.unwrap_or(""));
    let frames = l.text(Msg::CueFramesUnit);
    let suffix = format!(" {frames}");
    let inputs = &names.view.inputs;
    let titles: Vec<InputId> = names
        .project
        .titles()
        .iter()
        .map(|title| title.input)
        .collect();
    let pick = |ui: &mut Ui, salt: &str, input: &mut WireInputId, choices: &[InputId]| {
        let mut value = Some(*input);
        let changed = input_picker(ui, names, salt, (&mut value, choices), false, edit);
        if let Some(value) = value {
            *input = value;
        }
        changed.then_some(Change::Commit)
    };
    match action {
        WireCueAction::Preview { input } => pick(ui, "input", input, inputs),
        WireCueAction::Take {
            input,
            effect,
            duration_frames,
        } => {
            let mut change = input_picker(ui, names, "input", (input, inputs), true, edit)
                .then_some(Change::Commit);
            let stingers: Vec<_> = names
                .view
                .stingers
                .iter()
                .filter_map(|stinger| WireStingerSlotId::new(stinger.slot))
                .collect();
            let width = fit_width(ui, 96.0);
            ui.add_enabled_ui(edit.is_none(), |ui| {
                ComboBox::from_id_salt("effect")
                    .width(width)
                    .truncate()
                    .selected_text(
                        RichText::new(crate::transitions::effect_name(l, *effect))
                            .size(legible(11.0)),
                    )
                    .show_ui(ui, |ui| {
                        let effects = core::iter::once(TransitionFavoriteEffect::Cut)
                            .chain(AutoTransitionKind::ALL.map(TransitionFavoriteEffect::Mix))
                            .chain(
                                stingers
                                    .iter()
                                    .copied()
                                    .map(TransitionFavoriteEffect::Stinger),
                            );
                        for option in effects {
                            if ui
                                .selectable_label(
                                    *effect == option,
                                    crate::transitions::effect_name(l, option),
                                )
                                .on_hover_text(l.text(Msg::CueParamEffectHover))
                                .clicked()
                                && *effect != option
                            {
                                *effect = option;
                                *duration_frames = match option {
                                    TransitionFavoriteEffect::Cut => 0,
                                    _ => (*duration_frames).max(25),
                                };
                                change = Some(Change::Commit);
                            }
                        }
                    })
                    .response
                    .hint(l.text(Msg::CueParamEffectHover), edit.unwrap_or(""));
            });
            let cut = *effect == TransitionFavoriteEffect::Cut;
            let reason = edit.unwrap_or(Msg::PlaylistReasonCutHasNoLength.en());
            let mut frames_value = (*duration_frames).max(u32::from(cut));
            let number_change = number(
                ui,
                &mut frames_value,
                1..=MAX_CUE_TRANSITION_FRAMES,
                (l.text(Msg::CueParamFramesHover), &suffix),
                (edit.is_none() && !cut, reason),
            );
            if !cut {
                *duration_frames = frames_value;
            }
            number_change.or(change)
        }
        WireCueAction::OverlayOn {
            channel: slot,
            input,
        } => {
            let change = channel(ui, slot, l, edit);
            pick(ui, "input", input, inputs).or(change)
        }
        WireCueAction::OverlayOff { channel: slot } => channel(ui, slot, l, edit),
        WireCueAction::TitleOn {
            input,
            channel: slot,
        } => {
            let change = pick(ui, "title", input, &titles);
            channel(ui, slot, l, edit).or(change)
        }
        WireCueAction::TitleText { input, field, text } => {
            let before = *input;
            let mut change = pick(ui, "title", input, &titles);
            let title = names.project.title(input.to_domain());
            if *input != before
                && let Some(first) = title.and_then(|title| title.fields.first())
            {
                *field = first.field;
            }
            let fields = title
                .map(|title| title.fields.as_slice())
                .unwrap_or_default();
            let current = fields
                .iter()
                .find(|candidate| candidate.field == *field)
                .map(|candidate| candidate.name.clone())
                .unwrap_or_default();
            let width = fit_width(ui, 90.0);
            ui.add_enabled_ui(edit.is_none(), |ui| {
                ComboBox::from_id_salt("field")
                    .width(width)
                    .truncate()
                    .selected_text(RichText::new(current).size(legible(11.0)))
                    .show_ui(ui, |ui| {
                        for candidate in fields {
                            if ui
                                .selectable_label(candidate.field == *field, &candidate.name)
                                .on_hover_text(l.text(Msg::CueParamFieldHover))
                                .clicked()
                                && candidate.field != *field
                            {
                                *field = candidate.field;
                                change = Some(Change::Commit);
                            }
                        }
                    })
                    .response
                    .hint(l.text(Msg::CueParamFieldHover), edit.unwrap_or(""));
            });
            let width = fit_width(ui, 120.0);
            let response = ui
                .add_enabled(
                    edit.is_none(),
                    TextEdit::singleline(text).desired_width(width),
                )
                .hint(l.text(Msg::CueParamTextHover), edit.unwrap_or(""));
            if response.lost_focus() {
                change = Some(Change::Commit);
            }
            change
        }
        WireCueAction::TitleOff { input } => pick(ui, "title", input, &titles),
        WireCueAction::FadeToBlack {
            active,
            duration_frames,
        } => {
            let change = flag(ui, active, l.text(Msg::CueParamActiveHover), edit);
            number(
                ui,
                duration_frames,
                1..=MAX_CUE_TRANSITION_FRAMES,
                (l.text(Msg::CueParamFramesHover), &suffix),
                enabled,
            )
            .or(change)
        }
        WireCueAction::PlaylistStart | WireCueAction::PlaylistStop => None,
        WireCueAction::AudioGain {
            input,
            gain_millidb,
        } => {
            let change = pick(ui, "input", input, inputs);
            let mut decibels = f64::from(*gain_millidb) / 1000.0;
            let gain = number(
                ui,
                &mut decibels,
                f64::from(MIN_GAIN_MILLIDB) / 1000.0..=f64::from(MAX_GAIN_MILLIDB) / 1000.0,
                (l.text(Msg::CueParamGainHover), " dB"),
                enabled,
            );
            #[allow(clippy::cast_possible_truncation)] // clamped to the strip range
            {
                *gain_millidb = (decibels * 1000.0)
                    .round()
                    .clamp(f64::from(MIN_GAIN_MILLIDB), f64::from(MAX_GAIN_MILLIDB))
                    as i32;
            }
            gain.or(change)
        }
        WireCueAction::AudioMute { input, muted } => {
            let change = pick(ui, "input", input, inputs);
            flag(ui, muted, l.text(Msg::CueParamMuteHover), edit).or(change)
        }
        WireCueAction::Record { recorder, start } => {
            let mut slot = recorder.get();
            let change = number(
                ui,
                &mut slot,
                1..=2,
                (l.text(Msg::CueParamRecorderHover), ""),
                enabled,
            );
            if let Some(value) = WireRecorderSlot::new(slot) {
                *recorder = value;
            }
            flag(ui, start, l.text(Msg::CueParamStartHover), edit).or(change)
        }
        WireCueAction::Stream { target, start } => {
            let streams = &names.view.streams;
            let current = streams
                .iter()
                .find(|stream| stream.target == target.get())
                .map(|stream| stream.name.clone())
                .unwrap_or_default();
            let mut change = None;
            let width = fit_width(ui, 100.0);
            ui.add_enabled_ui(edit.is_none(), |ui| {
                ComboBox::from_id_salt("target")
                    .width(width)
                    .truncate()
                    .selected_text(RichText::new(current).size(legible(11.0)))
                    .show_ui(ui, |ui| {
                        for stream in streams {
                            if ui
                                .selectable_label(stream.target == target.get(), &stream.name)
                                .on_hover_text(l.text(Msg::CueParamTargetHover))
                                .clicked()
                                && stream.target != target.get()
                            {
                                *target = WireStreamTargetId::new(stream.target);
                                change = Some(Change::Commit);
                            }
                        }
                    })
                    .response
                    .hint(l.text(Msg::CueParamTargetHover), edit.unwrap_or(""));
            });
            flag(ui, start, l.text(Msg::CueParamStartHover), edit).or(change)
        }
        WireCueAction::Media { input, operation } => {
            let media = media_inputs(names.view, names.project);
            let mut value = Some(*input);
            let changed = input_picker(ui, names, "media", (&mut value, &media), false, edit)
                .then_some(Change::Commit);
            if let Some(value) = value {
                *input = value;
            }
            if let WireCueMediaOperation::Seek { frame } = operation {
                number(
                    ui,
                    frame,
                    0..=u64::from(u32::MAX),
                    (l.text(Msg::CueParamSeekHover), ""),
                    enabled,
                )
                .or(changed)
            } else {
                changed
            }
        }
        WireCueAction::Wait { frames: wait } => number(
            ui,
            wait,
            1..=MAX_CUE_WAIT_FRAMES,
            (l.text(Msg::CueParamWaitHover), &suffix),
            enabled,
        ),
        WireCueAction::Delay { millis } => {
            let unit = format!(" {}", l.text(Msg::CueMillisUnit));
            number(
                ui,
                millis,
                1..=MAX_CUE_DELAY_MS,
                (l.text(Msg::CueParamDelayHover), &unit),
                enabled,
            )
        }
        WireCueAction::RunScript { script } => {
            let scripts = names.project.scripts();
            let hover = l.text(Msg::CueParamScriptHover);
            let current = scripts
                .iter()
                .find(|found| found.id == *script)
                .map_or_else(|| script.to_string(), |found| found.spec.name.clone());
            let mut change = None;
            let width = fit_width(ui, 110.0);
            ui.add_enabled_ui(edit.is_none(), |ui| {
                ComboBox::from_id_salt("script")
                    .width(width)
                    .truncate()
                    .selected_text(RichText::new(current).size(legible(11.0)))
                    .show_ui(ui, |ui| {
                        for found in scripts {
                            if ui
                                .selectable_label(found.id == *script, &found.spec.name)
                                .on_hover_text(hover)
                                .clicked()
                                && found.id != *script
                            {
                                *script = found.id;
                                change = Some(Change::Commit);
                            }
                        }
                    })
                    .response
                    .hint(hover, edit.unwrap_or(""));
            });
            change
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_protocol::CuePendingStatus;

    fn cue(id: u64) -> WireCue {
        WireCue {
            id,
            name: format!("Cue {id}"),
            auto_follow: false,
            actions: Vec::new(),
        }
    }

    fn list(id: u64, state: CueRunStatus) -> CueListStatus {
        CueListStatus {
            id,
            spec: CueListSpec {
                name: "Show".into(),
                cues: vec![cue(10), cue(20), cue(30)],
            },
            state,
        }
    }

    fn named(id: u64, name: &str) -> CueListStatus {
        let mut status = list(id, CueRunStatus::default());
        status.spec.name = name.into();
        status
    }

    fn drafted(panel: &CuePanel) -> &str {
        &panel.draft.as_ref().unwrap().spec.name
    }

    /// The working copy keeps an edit that was sent until its echo lands,
    /// keeps later edits across the echo of an earlier one, snaps back to
    /// the replicated list once an edit resolves without an echo (it was
    /// refused), and follows another client's edit.
    #[test]
    fn the_working_copy_survives_echoes_and_snaps_back_after_a_refusal() {
        let mut panel = CuePanel::default();
        panel.sync_draft(&named(1, "Show"), false);
        panel.draft.as_mut().unwrap().spec.name = "One".into();
        assert!(matches!(
            panel.commit_draft(),
            Some(CueIntent::Set { list: 1, spec }) if spec.name == "One"
        ));
        assert_eq!(
            panel.commit_draft(),
            None,
            "an unchanged copy is not resent"
        );
        // The edit is in flight: the stale replicated list must not undo it.
        panel.sync_draft(&named(1, "Show"), true);
        assert_eq!(drafted(&panel), "One");
        panel.draft.as_mut().unwrap().spec.name = "Two".into();
        assert!(panel.commit_draft().is_some());
        // The first edit's echo keeps the second.
        panel.sync_draft(&named(1, "One"), true);
        assert_eq!(drafted(&panel), "Two");
        panel.sync_draft(&named(1, "Two"), false);
        assert_eq!(drafted(&panel), "Two");
        // A refused edit resolves with no echo and snaps back.
        panel.draft.as_mut().unwrap().spec.name = "Three".into();
        assert!(panel.commit_draft().is_some());
        panel.sync_draft(&named(1, "Two"), true);
        assert_eq!(drafted(&panel), "Three");
        panel.sync_draft(&named(1, "Two"), false);
        assert_eq!(drafted(&panel), "Two");
        // Another client's edit replaces the copy.
        panel.sync_draft(&named(1, "Theirs"), false);
        assert_eq!(drafted(&panel), "Theirs");
        // So does showing another list.
        panel.sync_draft(&named(2, "Other"), false);
        assert_eq!(drafted(&panel), "Other");
    }

    /// A draft whose waitless auto-follow (or one cue) would take Program
    /// twice on one frame flags the offending cue with the cue whose take
    /// it collides with, and is held as a draft rather than sent.
    #[test]
    fn a_draft_taking_program_twice_on_one_frame_is_flagged_and_held() {
        let take = WireCueAction::Take {
            input: None,
            effect: TransitionFavoriteEffect::Cut,
            duration_frames: 0,
        };
        let wait = WireCueAction::Wait { frames: 1 };
        let mut cues = vec![cue(10), cue(20), cue(30), cue(40)];
        cues[0].actions = vec![take.clone()];
        cues[0].auto_follow = true;
        cues[1].actions = vec![WireCueAction::PlaylistStop];
        cues[1].auto_follow = true;
        cues[2].actions = vec![WireCueAction::PlaylistStart, wait.clone(), take.clone()];
        cues[3].actions = vec![take.clone(), take.clone()];
        assert_eq!(
            take_conflicts(&cues),
            vec![(30, 10), (40, 40)],
            "(offending cue, cue it collides with)"
        );
        cues[1].actions.push(wait);
        cues[3].actions.pop();
        assert_eq!(take_conflicts(&cues), Vec::new());

        let mut panel = CuePanel::default();
        panel.sync_draft(&list(1, CueRunStatus::default()), false);
        let draft = &mut panel.draft.as_mut().unwrap().spec.cues;
        draft[0].actions = vec![take.clone()];
        draft[0].auto_follow = true;
        draft[1].actions = vec![take];
        assert_eq!(panel.commit_draft(), None, "a conflicting draft is held");
        panel.draft.as_mut().unwrap().spec.cues[0].auto_follow = false;
        assert!(panel.commit_draft().is_some());
    }

    #[test]
    fn roles_rank_fault_and_wait_over_live_and_standby() {
        let state = CueRunStatus {
            standby: Some(20),
            live: Some(10),
            pending: None,
            played: vec![10],
            fault: None,
            armed: None,
        };
        assert_eq!(CueRole::of(&state, 10), CueRole::Live);
        assert_eq!(CueRole::of(&state, 20), CueRole::Standby);
        assert_eq!(CueRole::of(&state, 30), CueRole::Idle);
        let waiting = CueRunStatus {
            pending: Some(CuePendingStatus {
                cue: 10,
                action: 1,
                due_frame: 50,
            }),
            ..state.clone()
        };
        assert_eq!(CueRole::of(&waiting, 10), CueRole::Waiting);
        let fault = CueRunStatus {
            fault: Some(CueFaultStatus {
                cue: 10,
                action: 0,
                detail: fm_protocol::FaultDetail {
                    reason: None,
                    message: "refused".into(),
                },
            }),
            ..state
        };
        assert_eq!(CueRole::of(&fault, 10), CueRole::Fault);
        let played = CueRunStatus {
            played: vec![10],
            ..CueRunStatus::default()
        };
        assert_eq!(CueRole::of(&played, 10), CueRole::Played);
        // Protocol 2.64: an armed GO outranks standby and played, not live.
        let armed = CueRunStatus {
            armed: Some(fm_protocol::CueArmedStatus {
                cue: 20,
                due_frame: 90,
            }),
            ..played
        };
        assert_eq!(CueRole::of(&armed, 20), CueRole::Armed);
        assert_eq!(
            cue_remove_block(&list(1, armed.clone()), 20, None),
            Some(Msg::CueReasonArmed.en())
        );
        assert_eq!(cue_remove_block(&list(1, armed), 10, None), None);
    }

    /// Protocol 2.64: the arm delay is whole seconds, `MM:SS`, or
    /// `HH:MM:SS`, from one second to 24 hours.
    #[test]
    fn arm_delays_parse_from_one_second_to_a_day() {
        assert_eq!(parse_arm_delay("5"), Some(5_000));
        assert_eq!(parse_arm_delay(" 1:30 "), Some(90_000));
        assert_eq!(parse_arm_delay("01:00:00"), Some(3_600_000));
        assert_eq!(parse_arm_delay("24:00:00"), Some(86_400_000));
        assert_eq!(parse_arm_delay("86400"), Some(86_400_000));
        for bad in [
            "", "0", "24:00:01", "86401", "1:60", "1:60:00", "a", "1::2", "-3", "1:2:3:4",
        ] {
            assert_eq!(parse_arm_delay(bad), None, "{bad}");
        }
    }

    /// Protocol 2.63: a deferred action's fault says what the server refused
    /// it with, a typed reason in the console language, else the server's
    /// message.
    #[test]
    fn a_fault_note_says_what_the_server_refused_the_action_with() {
        let state = StudioUiState::new(crate::StudioConnectionStatus::Ready);
        let mut fault = CueFaultStatus {
            cue: 30,
            action: 1,
            detail: fm_protocol::FaultDetail {
                reason: Some(fm_protocol::RefusalReason::StingerRealizing { slot: 2 }),
                message: "Stinger slot 2 has not finished realizing".into(),
            },
        };
        assert_eq!(
            fault_note(Locale::ENGLISH, &state, &fault),
            "Cue 30 stopped at action 2: the authority refused it. The server said: \
             Stinger 2 is still being prepared; try again in a moment."
        );
        let spanish = fault_note(
            Locale::new(fm_ui_model::i18n::Language::Spanish),
            &state,
            &fault,
        );
        assert!(
            spanish.contains("El Stinger 2 aún se está preparando"),
            "{spanish}"
        );
        fault.detail.reason = None;
        assert!(
            fault_note(Locale::ENGLISH, &state, &fault)
                .ends_with("The server said: Stinger slot 2 has not finished realizing"),
        );
    }

    #[test]
    fn go_needs_a_standby_cue_and_the_session_gate() {
        let ready = list(
            7,
            CueRunStatus {
                standby: Some(10),
                ..CueRunStatus::default()
            },
        );
        assert_eq!(
            go_intent(&ready, None),
            Ok(CueIntent::Control {
                list: 7,
                operation: CueOperation::Go { cue: None },
            })
        );
        assert_eq!(go_intent(&ready, Some("blocked")), Err("blocked"));
        let done = list(7, CueRunStatus::default());
        assert_eq!(go_intent(&done, None), Err(Msg::CueReasonNoStandby.en()));
    }

    /// GO and a row's fire key run the cue and every cue it follows into;
    /// the authority checks that whole chain up front, so each needs the
    /// permissions its actions need.
    #[test]
    fn firing_needs_the_permissions_of_the_chain_it_runs() {
        let wire = |value: u128| {
            WireInputId::from_domain(InputId::new(core::num::NonZeroU128::new(value).unwrap()))
        };
        let mut status = list(7, CueRunStatus::default());
        status.spec.cues[0].auto_follow = true;
        status.spec.cues[0].actions = vec![WireCueAction::Take {
            input: Some(wire(2)),
            effect: fm_protocol::TransitionFavoriteEffect::Cut,
            duration_frames: 0,
        }];
        status.spec.cues[1].actions = vec![WireCueAction::AudioMute {
            input: wire(2),
            muted: true,
        }];
        status.spec.cues[2].actions = vec![
            WireCueAction::Preview { input: wire(1) },
            WireCueAction::TitleText {
                input: wire(3),
                field: fm_protocol::WireTitleFieldId::new(core::num::NonZeroU128::MIN),
                text: "x".into(),
            },
        ];
        let operator = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_switcher_permissions(true, true)
            .with_audio_permission(true);
        let transition_only = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_switcher_permissions(false, true);

        // Cue 10 follows into cue 20's mute.
        assert_eq!(chain_block(&operator, &[], &status, 10), None);
        assert_eq!(
            chain_block(&transition_only, &[], &status, 10),
            Some(Msg::CueReasonChainAudio.en())
        );
        assert_eq!(
            chain_block(&transition_only, &[], &status, 20),
            Some(Msg::CueReasonChainAudio.en())
        );
        // Cue 30 selects Preview and sets title text.
        assert_eq!(
            chain_block(&transition_only, &[], &status, 30),
            Some(Msg::CueReasonChainPreview.en())
        );
        assert_eq!(
            chain_block(&operator, &[], &status, 30),
            Some(Msg::CueReasonChainTitle.en())
        );
        let admin = operator.clone().with_edit_project_permission(true);
        assert_eq!(chain_block(&admin, &[], &status, 30), None);
        // Cue 20 does not follow on into cue 30.
        status.spec.cues[2].actions.clear();
        assert_eq!(chain_block(&operator, &[], &status, 20), None);
        // A script step needs what every verb of its script needs, in any
        // branch (Protocol 2.60).
        let script = |source: &str| fm_protocol::ScriptStatus {
            id: 5,
            spec: fm_protocol::ScriptSpec {
                name: "Open".into(),
                source: source.into(),
            },
        };
        status.spec.cues[2].actions = vec![WireCueAction::RunScript { script: 5 }];
        let muting = [script("if program == 9\n  mute 2\nend\ncut 1\n")];
        assert_eq!(
            chain_block(&transition_only, &muting, &status, 30),
            Some(Msg::CueReasonChainAudio.en())
        );
        assert_eq!(chain_block(&operator, &muting, &status, 30), None);
        let titling = [script("title_text 3, 1, \"x\"\n")];
        assert_eq!(
            chain_block(&operator, &titling, &status, 30),
            Some(Msg::CueReasonChainTitle.en())
        );
        assert_eq!(
            chain_block(&transition_only, &[script("cut 1\n")], &status, 30),
            None
        );
        // An unknown script or a syntax error needs nothing more: the run
        // fails at the authority.
        assert_eq!(chain_block(&transition_only, &[], &status, 30), None);
        assert_eq!(
            chain_block(&transition_only, &[script("mute 1\nif\n")], &status, 30),
            None
        );
        // Every reason is translated.
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        for msg in [
            Msg::CueReasonChainPreview,
            Msg::CueReasonChainAudio,
            Msg::CueReasonChainTitle,
            Msg::CueReasonNoScriptsLeft,
            Msg::CueParamScriptHover,
            Msg::CueActRunScript,
            Msg::ScriptReasonRunByCue,
        ] {
            assert_ne!(spanish.text(msg), msg.en());
        }
    }

    /// Regression: the `G` key fired the standby cue whatever its chain
    /// needed, while the GO key beside it was disabled because a script
    /// step's script mutes a strip the session may not touch.
    #[test]
    fn the_g_key_fires_only_a_chain_the_go_key_would() {
        let mut status = list(
            7,
            CueRunStatus {
                standby: Some(10),
                ..CueRunStatus::default()
            },
        );
        status.spec.cues[0].actions = vec![WireCueAction::RunScript { script: 5 }];
        let muting = [ScriptStatus {
            id: 5,
            spec: fm_protocol::ScriptSpec {
                name: "Open".into(),
                source: "if program == 9\n  mute 2\nend\ncut 1\n".into(),
            },
        }];
        let transition_only = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_switcher_permissions(false, true);
        let operator = transition_only.clone().with_audio_permission(true);
        assert_eq!(go_key_list(&transition_only, &muting, &status), None);
        assert!(go_intent(&status, chain_block(&transition_only, &muting, &status, 10)).is_err());
        assert_eq!(go_key_list(&operator, &muting, &status), Some(7));
        status.state.standby = None;
        assert_eq!(go_key_list(&operator, &muting, &status), None);
    }

    #[test]
    fn new_ids_take_the_lowest_free_list_and_the_next_cue() {
        let lists = vec![
            list(1, CueRunStatus::default()),
            list(3, CueRunStatus::default()),
        ];
        assert_eq!(new_list_id(&lists), Some(2));
        let full: Vec<_> = (1..=8)
            .map(|id| list(id, CueRunStatus::default()))
            .collect();
        assert_eq!(new_list_id(&full), None);
        assert_eq!(new_cue_id(&[]), 1);
        assert_eq!(new_cue_id(&[cue(4), cue(9), cue(2)]), 10);
    }

    #[test]
    fn every_action_kind_round_trips_its_kind_and_has_a_label() {
        for kind in ActionKind::ALL {
            assert_ne!(kind.label(), "");
        }
        assert_eq!(
            ActionKind::of(&WireCueAction::Wait { frames: 3 }),
            ActionKind::Wait
        );
        assert_eq!(
            ActionKind::of(&WireCueAction::PlaylistStop),
            ActionKind::PlaylistStop
        );
        assert_eq!(
            ActionKind::of(&WireCueAction::Delay { millis: 40 }),
            ActionKind::Delay
        );
        assert_eq!(
            ActionKind::of(&WireCueAction::RunScript { script: 2 }),
            ActionKind::RunScript
        );
        let clip = WireInputId::new(core::num::NonZeroU128::new(4).unwrap());
        for (operation, kind) in [
            (WireCueMediaOperation::Play, ActionKind::MediaPlay),
            (WireCueMediaOperation::Pause, ActionKind::MediaPause),
            (
                WireCueMediaOperation::Seek { frame: 9 },
                ActionKind::MediaSeek,
            ),
            (WireCueMediaOperation::Restart, ActionKind::MediaRestart),
        ] {
            assert_eq!(
                ActionKind::of(&WireCueAction::Media {
                    input: clip,
                    operation
                }),
                kind
            );
        }
    }
}
