//! Replicated operator cue lists (Protocol 2.50, RC-017; media transports
//! Protocol 2.51).
//!
//! A snapshot announces one `cue_list_state` per list; `cue_list_changed`
//! and `cue_list_removed` replace or drop a whole list after an edit, and
//! `cue_run_changed` carries a live operation's run state together with
//! every desired state its fired actions may have changed and what each
//! script step it ran did (Protocol 2.61), which becomes that script's
//! last run.

use fm_protocol::{
    CueListStatus, CueMediaTransport, CueRunChange, CueRunStatus, CueScriptRun, CueTitleText,
    FadeToBlackState, PlaylistRunState, RecordingStatus, TitleFieldValue, WireCueAction,
};
use fm_types::InputId;

use crate::{
    BusSelection, InputAudioStripStatus, ManualTransitionStatus, ModelError, OverlayStatus,
    ProjectState, StreamStatus,
};

/// A live cue operation's effect as a durable change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CueRunUpdate {
    pub list: u64,
    /// The frame the fired actions reach Program on.
    pub frame: u64,
    pub state: CueRunStatus,
    pub fired: Vec<u64>,
    pub selection: BusSelection,
    pub manual_transition: ManualTransitionStatus,
    pub fade_to_black: FadeToBlackState,
    pub overlays: Vec<OverlayStatus>,
    pub input_audio_strips: Vec<InputAudioStripStatus>,
    pub streams: Vec<StreamStatus>,
    pub recordings: Vec<RecordingStatus>,
    pub playlist: PlaylistRunState,
    pub titles: Vec<CueTitleText>,
    /// The transports the fired media actions left (Protocol 2.51).
    pub media: Vec<CueMediaTransport>,
    /// Every script step the run ran, in order (Protocol 2.61); empty for
    /// a direct script run's effects.
    pub scripts: Vec<CueScriptRun>,
}

impl CueRunUpdate {
    pub(crate) fn from_protocol(change: CueRunChange) -> Self {
        Self {
            list: change.list,
            frame: change.frame,
            state: change.state,
            fired: change.fired,
            selection: BusSelection::new(change.program.to_domain(), change.preview.to_domain()),
            manual_transition: ManualTransitionStatus::from_protocol(change.manual_transition),
            fade_to_black: change.fade_to_black,
            overlays: crate::protocol_overlays(change.overlays),
            input_audio_strips: crate::protocol_input_audio_strips(change.input_audio_strips),
            streams: crate::protocol_streams(change.streams),
            recordings: change.recordings,
            playlist: change.playlist,
            titles: change.titles,
            media: change.media,
            scripts: change.scripts,
        }
    }
}

/// Every input a list's actions name.
pub fn list_inputs(list: &CueListStatus) -> impl Iterator<Item = InputId> + '_ {
    list.spec
        .cues
        .iter()
        .flat_map(|cue| cue.actions.iter())
        .filter_map(|action| match action {
            WireCueAction::Preview { input }
            | WireCueAction::OverlayOn { input, .. }
            | WireCueAction::TitleOn { input, .. }
            | WireCueAction::TitleText { input, .. }
            | WireCueAction::TitleNumber { input, .. }
            | WireCueAction::TitleOff { input }
            | WireCueAction::AudioGain { input, .. }
            | WireCueAction::AudioMute { input, .. }
            | WireCueAction::Media { input, .. } => Some(input.to_domain()),
            WireCueAction::Take { input, .. } => input.map(fm_protocol::WireInputId::to_domain),
            _ => None,
        })
}

fn state_is_valid(list: &CueListStatus, state: &CueRunStatus) -> bool {
    let holds = |cue: u64| list.spec.cues.iter().any(|candidate| candidate.id == cue);
    state.standby.is_none_or(holds)
        && state.live.is_none_or(holds)
        && state.pending.is_none_or(|pending| holds(pending.cue))
        && state.fault.as_ref().is_none_or(|fault| holds(fault.cue))
        && state.played.iter().all(|cue| holds(*cue))
        && state.armed.is_none_or(|armed| holds(armed.cue))
}

/// Checks a whole list against the replicated project.
pub(crate) fn validate_list(list: &CueListStatus, state: &ProjectState) -> Result<(), ModelError> {
    if list.id == 0 || !state_is_valid(list, &list.state) {
        return Err(ModelError::InvalidCueList(list.id));
    }
    if let Some(input) = list_inputs(list).find(|input| !state.inputs.contains(input)) {
        return Err(ModelError::UnknownInput(input));
    }
    Ok(())
}

/// Checks the cue-specific parts of a run update; the caller validates its
/// switcher state like a `desired_switcher`.
pub(crate) fn validate_run(update: &CueRunUpdate, state: &ProjectState) -> Result<(), ModelError> {
    let list = state
        .cue_lists
        .iter()
        .find(|list| list.id == update.list)
        .ok_or(ModelError::InvalidCueList(update.list))?;
    if !state_is_valid(list, &update.state)
        || update.scripts.len() > fm_protocol::MAX_CUE_LIST_SCRIPT_STEPS
    {
        return Err(ModelError::InvalidCueList(update.list));
    }
    if let Some(run) = update
        .scripts
        .iter()
        .find(|run| state.script(run.script).is_none())
    {
        return Err(ModelError::InvalidScript(run.script));
    }
    validate_effects(update, state)
}

/// Checks the desired state a run update carries (a cue run's or a script
/// run's, RC-011) beyond its switcher state.
pub(crate) fn validate_effects(
    update: &CueRunUpdate,
    state: &ProjectState,
) -> Result<(), ModelError> {
    crate::validate_streams(&update.streams)?;
    crate::validate_recordings(&update.recordings)?;
    if let Some(transport) = update
        .media
        .iter()
        .find(|transport| !state.inputs.contains(&transport.input.to_domain()))
    {
        return Err(ModelError::InvalidMediaChange(transport.input.to_domain()));
    }
    for title in &update.titles {
        let input = title.input.to_domain();
        let found = state.title(input).ok_or(ModelError::UnknownTitle(input))?;
        if title.field_revision < found.field_revision
            || !found.fields.iter().any(|field| field.field == title.field)
        {
            return Err(ModelError::InvalidTitleChange(input));
        }
    }
    Ok(())
}

/// Applies a validated run update.
pub(crate) fn apply_run(state: &mut ProjectState, mut update: CueRunUpdate) {
    let list = update.list;
    let run = update.state.clone();
    for ran in core::mem::take(&mut update.scripts) {
        crate::scripts::record_run(
            state,
            crate::ScriptRunReport {
                script: ran.script,
                frame: update.frame,
                actions: ran.actions,
                instructions: ran.instructions,
                cue_list: Some(list),
            },
        );
    }
    state.cue_frame = Some(
        state
            .cue_frame
            .map_or(update.frame, |seen| seen.max(update.frame)),
    );
    apply_effects(state, update);
    if let Some(found) = state.cue_lists.iter_mut().find(|found| found.id == list) {
        found.state = run;
    }
}

/// Applies the desired state of a validated run update (a cue run's or a
/// script run's).
pub(crate) fn apply_effects(state: &mut ProjectState, update: CueRunUpdate) {
    state.switcher.desired = update.selection;
    state.switcher.desired_manual_transition = update.manual_transition;
    state.switcher.desired_fade_to_black = update.fade_to_black;
    state.desired_overlays = update.overlays;
    state.input_audio_strips = update.input_audio_strips;
    state.streams = update.streams;
    state.recordings = update.recordings;
    if let Some(playlist) = state.playlist.as_mut()
        && playlist.state != update.playlist
    {
        playlist.state = update.playlist;
        playlist.frame = update.frame;
    }
    for transport in update.media {
        if let Some(status) = state.media.get_mut(&transport.input.to_domain()) {
            status.transport.state = transport.state;
            status.transport.position = transport.position;
            status.transport.position_time = transport.position_time;
        }
    }
    for text in update.titles {
        if let Some(title) = state
            .titles
            .iter_mut()
            .find(|title| title.input == text.input.to_domain())
        {
            title.field_revision = text.field_revision;
            if let Some(field) = title
                .fields
                .iter_mut()
                .find(|field| field.field == text.field)
            {
                field.value = match text.value {
                    fm_protocol::CueTitleValue::Text(text) => TitleFieldValue::Text(text),
                    fm_protocol::CueTitleValue::Number(number) => TitleFieldValue::Number(number),
                };
            }
        }
    }
}

/// Inserts or replaces a whole list, keeping project order (new lists are
/// appended).
pub(crate) fn apply_list(state: &mut ProjectState, list: CueListStatus) {
    match state
        .cue_lists
        .iter_mut()
        .find(|candidate| candidate.id == list.id)
    {
        Some(current) => *current = list,
        None => state.cue_lists.push(list),
    }
}
