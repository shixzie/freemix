//! Input lifecycle edits (IN-026): duplicate, replace source, category and
//! colour label, pause and resume, close and reopen, the category list,
//! batch media import, and batch removal.
//!
//! Every edit is a project edit: it honours the padlock (the project lock,
//! and the padlock of every input it changes) and, except batch removal,
//! is recorded in the undo history with an exact inverse:
//!
//! - duplicate and import are undone by removing exactly the inputs they
//!   appended (which must still be the last inputs, under the names they
//!   were given, ungraded); redo appends them again;
//! - replace source, category, colour, pause, and close are undone by
//!   restoring the previous value;
//! - the category list is undone by restoring the previous list.
//!
//! Batch removal, like single removal, has no inverse: it forgets the
//! history entries naming the removed inputs.
//!
//! A *paused* input holds its last frame: its media transport does not
//! advance and the media plane stops pulling its capture. A *closed* input
//! releases its source (the engine closes its media transport; the media
//! plane releases its decoder or capture) and renders black and silent; it
//! keeps its definition, labels, strip, and references, and reopening
//! restores it. Closing is refused while the input is on air, directly or
//! as an enabled layer of a scene on air.
//!
//! A closed input never reaches Preview or air: any command that would bring
//! it there (selecting it or a scene showing it to Preview, cutting or
//! transitioning to it, keying it, a playlist cue) is refused as
//! `input_closed` ([`ClosedInputGuard`]). Closing the input on Preview is
//! allowed; it then cannot be taken until it reopens. So whatever its kind,
//! a closed input is never seen on Program or an output; where it is still
//! shown (Preview, multiview), a closed media input shows no picture.

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_model::{
    BatchImportRefusal, BatchImportRequest, InputCategory, InputCategoryId, InputColorLabel,
    InputLifecycle, InputSourceError, InputSourceSpec, MEDIA_ASSET_URI_PREFIX,
};
use fm_types::InputId;
use std::collections::BTreeSet;

use crate::{EngineCommand, EngineEvent, MAX_SHOW_INPUTS, ShowState};

/// The inputs a show presents: on air (Program, a transition or manual
/// transition endpoint, a keyed or fading overlay, a playing Stinger's
/// media) and on Preview, each with every input a scene among them shows
/// as an enabled layer, through nested scenes.
struct ShownInputs {
    air: BTreeSet<InputId>,
    preview: BTreeSet<InputId>,
}

impl ShownInputs {
    fn of(state: &ShowState) -> Self {
        let switcher = state.desired_switcher();
        let air = state
            .inputs()
            .iter()
            .copied()
            .filter(|input| switcher.on_air_usage(*input).is_some());
        Self {
            air: with_scene_layers(state, air),
            preview: with_scene_layers(state, [switcher.preview()]),
        }
    }
}

/// `roots` and every input the scenes among them show as enabled layers,
/// through nested scenes.
fn with_scene_layers(
    state: &ShowState,
    roots: impl IntoIterator<Item = InputId>,
) -> BTreeSet<InputId> {
    let mut shown = BTreeSet::new();
    let mut scenes = BTreeSet::new();
    let mut pending: Vec<InputId> = roots.into_iter().collect();
    let mut pending_scenes = Vec::new();
    loop {
        if let Some(input) = pending.pop() {
            if shown.insert(input)
                && let Some(scene) = state.scene_input(input)
            {
                pending_scenes.push(scene.id());
            }
            continue;
        }
        let Some(scene) = pending_scenes.pop() else {
            break;
        };
        if !scenes.insert(scene) {
            continue;
        }
        let Some(scene) = state.scene(scene) else {
            continue;
        };
        for layer in scene.scene.layers.iter().filter(|layer| layer.enabled) {
            match layer.source {
                fm_model::SourceRef::Input(input) => pending.push(input),
                fm_model::SourceRef::Scene(nested) => pending_scenes.push(nested),
            }
        }
    }
    shown
}

/// Refuses a command that brings a closed input to Preview or air (IN-026):
/// captured before the command, checked after it. A closed input already on
/// Preview (closed there) may stay, but it is not taken to air.
pub(crate) struct ClosedInputGuard(Option<ShownInputs>);

impl ClosedInputGuard {
    /// Captures what `state` presents, when a command could bring a closed
    /// input to it: some input is closed, or the command is a lifecycle
    /// edit (the only kind that closes one).
    pub(crate) fn before(state: &ShowState, lifecycle_edit: bool) -> Self {
        let any_closed = state
            .input_lifecycles()
            .values()
            .any(|lifecycle| lifecycle.closed);
        Self((any_closed || lifecycle_edit).then(|| ShownInputs::of(state)))
    }

    /// Refuses `state` (after the command) when a closed input is newly on
    /// air or newly on Preview.
    pub(crate) fn check(self, state: &ShowState) -> Result<(), Rejection> {
        let Some(before) = self.0 else {
            return Ok(());
        };
        let closed: Vec<InputId> = state
            .input_lifecycles()
            .iter()
            .filter(|(_, lifecycle)| lifecycle.closed)
            .map(|(input, _)| *input)
            .collect();
        if closed.is_empty() {
            return Ok(());
        }
        let after = ShownInputs::of(state);
        let reached = closed.into_iter().find_map(|input| {
            if after.air.contains(&input) && !before.air.contains(&input) {
                Some((input, "air"))
            } else if after.preview.contains(&input) && !before.preview.contains(&input) {
                Some((input, "Preview"))
            } else {
                None
            }
        });
        match reached {
            Some((input, place)) => Err(rejection(
                RejectionCode::Conflict,
                "input",
                "input_closed",
                format!("input {input} is closed; reopen it before taking it to {place}"),
            )),
            None => Ok(()),
        }
    }
}

/// One input lifecycle edit (IN-026).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineInputLifecycleEdit {
    /// Appends a copy of `source` as `input`: the same source, Master
    /// strip, category, colour, media settings, title, or scene. Without a
    /// name the copy is named after the source with a numeric suffix.
    Duplicate {
        source: InputId,
        input: InputId,
        name: Option<String>,
    },
    /// Points `input` at another source of a kind that has one.
    ReplaceSource {
        input: InputId,
        source: InputSourceSpec,
    },
    SetCategory {
        input: InputId,
        category: Option<InputCategoryId>,
    },
    SetColor {
        input: InputId,
        color: Option<InputColorLabel>,
    },
    SetPaused {
        input: InputId,
        paused: bool,
    },
    SetClosed {
        input: InputId,
        closed: bool,
    },
    /// Replaces the category list. A category still assigned to an input
    /// cannot be dropped.
    SetCategories {
        categories: Vec<InputCategory>,
    },
    /// Appends one media input per acceptable item, in one undoable step.
    /// Items are validated one by one; refused items are reported in the
    /// event and the command is refused only when no item is acceptable.
    Import {
        items: Vec<BatchImportRequest>,
    },
    /// Removes every listed input in one step (all or nothing).
    RemoveInputs {
        inputs: Vec<InputId>,
    },
}

impl EngineInputLifecycleEdit {
    /// The inputs whose padlock covers this edit.
    #[must_use]
    pub fn locked_inputs(&self) -> Vec<InputId> {
        match self {
            Self::ReplaceSource { input, .. }
            | Self::SetCategory { input, .. }
            | Self::SetColor { input, .. }
            | Self::SetPaused { input, .. }
            | Self::SetClosed { input, .. } => vec![*input],
            Self::RemoveInputs { inputs } => inputs.clone(),
            Self::Duplicate { .. } | Self::SetCategories { .. } | Self::Import { .. } => Vec::new(),
        }
    }

    /// Whether this edit names `input`.
    #[must_use]
    pub fn names_input(&self, input: InputId) -> bool {
        match self {
            Self::Duplicate {
                source,
                input: added,
                ..
            } => *source == input || *added == input,
            Self::Import { items } => items.iter().any(|item| item.input == input),
            other => other.locked_inputs().contains(&input),
        }
    }

    /// The single input this edit changes, when it changes exactly one
    /// existing input.
    #[must_use]
    pub const fn input(&self) -> Option<InputId> {
        match self {
            Self::ReplaceSource { input, .. }
            | Self::SetCategory { input, .. }
            | Self::SetColor { input, .. }
            | Self::SetPaused { input, .. }
            | Self::SetClosed { input, .. } => Some(*input),
            Self::Duplicate { .. }
            | Self::SetCategories { .. }
            | Self::Import { .. }
            | Self::RemoveInputs { .. } => None,
        }
    }
}

fn rejection(code: RejectionCode, field: &str, issue: &str, message: String) -> Rejection {
    Rejection::new(code, message.clone()).with_field(FieldIssue::new(field, issue, message))
}

fn unknown_input(input: InputId) -> Rejection {
    rejection(
        RejectionCode::NotFound,
        "input",
        "unknown_input",
        format!("input {input} is not part of this show"),
    )
    .with_reason(crate::error::unknown_input_reason(input))
}

fn require_input(state: &ShowState, input: InputId) -> Result<(), Rejection> {
    if state.inputs().contains(&input) {
        Ok(())
    } else {
        Err(unknown_input(input))
    }
}

/// The inputs an edit appends, with their names, and the import items it
/// refuses.
pub(crate) type AddedInputs = (Vec<(InputId, String)>, Vec<(usize, BatchImportRefusal)>);

/// The inputs `edit` (a duplicate or an import) appends to `state`, with
/// their names, as if the inputs in `excluded` were not in the show; and
/// the import items it refuses.
#[allow(clippy::too_many_lines)] // plans a duplicate and every import item
pub(crate) fn added_inputs(
    state: &ShowState,
    edit: &EngineInputLifecycleEdit,
    excluded: &[InputId],
) -> Result<AddedInputs, Rejection> {
    let existing: Vec<(InputId, &str)> = state
        .inputs()
        .iter()
        .zip(state.input_names())
        .filter(|(input, _)| !excluded.contains(input))
        .map(|(input, name)| (*input, name.as_str()))
        .collect();
    match edit {
        EngineInputLifecycleEdit::Duplicate {
            source,
            input,
            name,
        } => {
            let Some((_, source_name)) = existing.iter().find(|(id, _)| id == source) else {
                return Err(unknown_input(*source));
            };
            if existing.iter().any(|(id, _)| id == input) {
                return Err(rejection(
                    RejectionCode::Conflict,
                    "input",
                    "duplicate_input",
                    format!("input {input} already exists"),
                ));
            }
            if existing.len() >= MAX_SHOW_INPUTS {
                return Err(rejection(
                    RejectionCode::Conflict,
                    "input",
                    "too_many_inputs",
                    format!("a show holds at most {MAX_SHOW_INPUTS} inputs"),
                ));
            }
            let taken = |candidate: &str| existing.iter().any(|(_, name)| *name == candidate);
            let name = match name {
                Some(name) => {
                    if fm_types::validate_input_name(name).is_err() {
                        return Err(rejection(
                            RejectionCode::InvalidCommand,
                            "name",
                            "invalid_name",
                            "input name must be nonblank and within the name limit".to_owned(),
                        ));
                    }
                    if taken(name) {
                        return Err(rejection(
                            RejectionCode::Conflict,
                            "name",
                            "duplicate_name",
                            format!("input name `{name}` is already in use"),
                        ));
                    }
                    name.clone()
                }
                None => fm_model::unique_input_name(source_name, taken).ok_or_else(|| {
                    rejection(
                        RejectionCode::Conflict,
                        "name",
                        "duplicate_name",
                        "no free name derives from the source input's name".to_owned(),
                    )
                })?,
            };
            Ok((vec![(*input, name)], Vec::new()))
        }
        EngineInputLifecycleEdit::Import { items } => {
            if items.is_empty() || items.len() > fm_model::MAX_BATCH_IMPORT_ITEMS {
                return Err(rejection(
                    RejectionCode::InvalidCommand,
                    "items",
                    "invalid_batch",
                    format!(
                        "a batch import carries 1 to {} items",
                        fm_model::MAX_BATCH_IMPORT_ITEMS
                    ),
                ));
            }
            let plan = fm_model::plan_batch_import(&existing, MAX_SHOW_INPUTS, items);
            if plan.accepted.is_empty() {
                let mut refused = Rejection::new(
                    RejectionCode::InvalidCommand,
                    "no item of the batch import can be imported",
                );
                for (index, refusal) in &plan.refused {
                    refused = refused.with_field(FieldIssue::new(
                        format!("items[{index}]"),
                        refusal.token(),
                        refusal.to_string(),
                    ));
                }
                return Err(refused);
            }
            Ok((
                plan.accepted
                    .into_iter()
                    .map(|item| (item.input, item.name))
                    .collect(),
                plan.refused,
            ))
        }
        _ => Ok((Vec::new(), Vec::new())),
    }
}

fn source_rejection(error: InputSourceError, source: &InputSourceSpec) -> Rejection {
    if let InputSourceSpec::Media { asset_uri } = source
        && !asset_uri.starts_with(MEDIA_ASSET_URI_PREFIX)
    {
        return rejection(
            RejectionCode::InvalidCommand,
            "source",
            BatchImportRefusal::Unavailable.token(),
            BatchImportRefusal::Unavailable.to_string(),
        );
    }
    rejection(
        RejectionCode::InvalidCommand,
        "source",
        error.code(),
        error.to_string(),
    )
}

fn set_lifecycle(
    state: &mut ShowState,
    input: InputId,
    change: impl FnOnce(&mut InputLifecycle),
) -> Result<(), Rejection> {
    require_input(state, input)?;
    let mut lifecycle = state.input_lifecycle(input);
    change(&mut lifecycle);
    state.set_input_lifecycle_unchecked(input, lifecycle);
    Ok(())
}

fn catalogue(error: crate::InputCatalogueError) -> Rejection {
    let message = error.to_string();
    let (code, issue) = match error {
        crate::InputCatalogueError::UnknownInput(_) => (RejectionCode::NotFound, "unknown_input"),
        crate::InputCatalogueError::InUse { .. }
        | crate::InputCatalogueError::InPlaylist { .. }
        | crate::InputCatalogueError::InCueList { .. } => (RejectionCode::Conflict, "input_in_use"),
        crate::InputCatalogueError::DuplicateInput(_) => {
            (RejectionCode::Conflict, "duplicate_input")
        }
        crate::InputCatalogueError::DuplicateName => (RejectionCode::Conflict, "duplicate_name"),
        crate::InputCatalogueError::TooManyInputs { .. } => {
            (RejectionCode::Conflict, "too_many_inputs")
        }
        crate::InputCatalogueError::EmptyName | crate::InputCatalogueError::NameTooLong => {
            (RejectionCode::InvalidCommand, "invalid_name")
        }
    };
    rejection(code, "input", issue, message).with_reason(error.reason())
}

/// Applies `edit` to the desired show; returns its one durable event.
#[allow(clippy::too_many_lines)]
pub(crate) fn apply(
    state: &mut ShowState,
    edit: &EngineInputLifecycleEdit,
) -> Result<EngineEvent, Rejection> {
    let mut added = Vec::new();
    let mut refused = Vec::new();
    match edit {
        EngineInputLifecycleEdit::Duplicate { source, .. } => {
            let (inputs, _) = added_inputs(state, edit, &[])?;
            let (input, name) = inputs[0].clone();
            if state.capture_settings(*source).is_some() {
                crate::engine::check_capture_room(state)?;
            }
            let mut staged = state.clone();
            staged.add_input(input, name).map_err(catalogue)?;
            if let Some(strip) = state.input_audio_strip(*source) {
                staged
                    .set_input_audio_strip(input, strip)
                    .map_err(|_| unknown_input(input))?;
            }
            // The duplicate keeps the strip's group (AU-001) but not its
            // label, which names the original.
            staged
                .set_input_audio_group(input, state.input_audio_group(*source))
                .map_err(|_| unknown_input(input))?;
            let original = state.input_lifecycle(*source);
            staged.set_input_lifecycle_unchecked(
                input,
                InputLifecycle {
                    category: original.category,
                    color: original.color,
                    paused: false,
                    closed: false,
                },
            );
            if let Some(spec) = state.input_source(*source) {
                staged.set_input_source_unchecked(input, spec.clone());
            }
            if let Some(settings) = state.capture_settings(*source) {
                staged.set_capture_settings_unchecked(input, settings.clone());
            }
            if let Some(settings) = state.media_settings(*source) {
                staged.insert_media_input(input, *settings);
            }
            if let Some(title) = state.title(*source) {
                staged.insert_title(input, title.clone());
            }
            if let Some(scene) = state.scene_of_input(*source) {
                staged.insert_scene_input(input, scene);
            }
            if let Some(target) = state.input_aliases().get(source) {
                staged.insert_input_alias(input, *target);
            }
            *state = staged;
            added = inputs;
        }
        EngineInputLifecycleEdit::ReplaceSource { input, source } => {
            require_input(state, *input)?;
            source
                .validate()
                .map_err(|error| source_rejection(error, source))?;
            let Some(current) = state.input_source(*input).cloned() else {
                return Err(rejection(
                    RejectionCode::Conflict,
                    "input",
                    "no_replaceable_source",
                    format!(
                        "input {input} is a scene, alias, title, mix feed, or colour input \
                         without a replaceable source"
                    ),
                ));
            };
            if let InputSourceSpec::Network(config) = source {
                crate::engine::check_listen_port(state, *input, config)?;
            }
            if matches!(source, InputSourceSpec::Device { .. })
                && state.capture_settings(*input).is_none()
            {
                crate::engine::check_capture_room(state)?;
            }
            if matches!(source, InputSourceSpec::SystemAudio(_))
                && !matches!(current, InputSourceSpec::SystemAudio(_))
            {
                crate::engine::check_system_audio_room(state)?;
            }
            match (current.is_media(), source.is_media()) {
                (false, true) => {
                    state.insert_media_input(*input, fm_model::MediaPlaybackSettings::default());
                }
                (true, false) => {
                    // A cue's media action needs its input to stay a media
                    // input (RC-017).
                    if let Some((list, cue)) = state.cue_lists().iter().find_map(|list| {
                        list.cues
                            .iter()
                            .find(|cue| {
                                cue.actions.iter().any(|action| {
                                    matches!(action, fm_model::CueAction::Media { input: named, .. } if named == input)
                                })
                            })
                            .map(|cue| (list.id, cue.id))
                    }) {
                        let error = crate::InputCatalogueError::InCueList {
                            input: *input,
                            list,
                            cue,
                        };
                        return Err(rejection(
                            RejectionCode::Conflict,
                            "source",
                            "input_in_use",
                            format!(
                                "input {input} must keep a media source: cue {cue} of cue list \
                                 {list} runs its media transport"
                            ),
                        )
                        .with_reason(error.reason()));
                    }
                    state.remove_media_input(*input);
                }
                _ => {}
            }
            state.set_input_source_unchecked(*input, source.clone());
        }
        EngineInputLifecycleEdit::SetCategory { input, category } => {
            if let Some(category) = category
                && !state
                    .input_categories()
                    .iter()
                    .any(|candidate| candidate.id == *category)
            {
                return Err(rejection(
                    RejectionCode::NotFound,
                    "category",
                    "unknown_category",
                    format!("category {category} does not exist"),
                )
                .with_reason(fm_command::RefusalReason::UnknownId {
                    kind: fm_command::IdKind::Category,
                    id: u128::from(category.get()),
                }));
            }
            set_lifecycle(state, *input, |lifecycle| lifecycle.category = *category)?;
        }
        EngineInputLifecycleEdit::SetColor { input, color } => {
            set_lifecycle(state, *input, |lifecycle| lifecycle.color = *color)?;
        }
        EngineInputLifecycleEdit::SetPaused { input, paused } => {
            set_lifecycle(state, *input, |lifecycle| lifecycle.paused = *paused)?;
        }
        EngineInputLifecycleEdit::SetClosed { input, closed } => {
            if *closed && let Some(usage) = state.desired_switcher().on_air_usage(*input) {
                return Err(rejection(
                    RejectionCode::Conflict,
                    "input",
                    "input_on_air",
                    format!("input {input} cannot be closed while it is {usage}"),
                )
                .with_reason(fm_command::RefusalReason::InputInUse {
                    input: input.get().get(),
                    usage: crate::error::input_use(usage),
                }));
            }
            if *closed && ShownInputs::of(state).air.contains(input) {
                return Err(rejection(
                    RejectionCode::Conflict,
                    "input",
                    "input_in_use",
                    format!("input {input} cannot be closed while a scene on air shows it"),
                )
                .with_reason(fm_command::RefusalReason::InputInUse {
                    input: input.get().get(),
                    usage: fm_command::InputUse::SceneOnAir,
                }));
            }
            set_lifecycle(state, *input, |lifecycle| lifecycle.closed = *closed)?;
        }
        EngineInputLifecycleEdit::SetCategories { categories } => {
            fm_model::validate_input_categories(categories).map_err(|error| {
                rejection(
                    RejectionCode::InvalidCommand,
                    "categories",
                    error.code(),
                    error.to_string(),
                )
            })?;
            for (input, lifecycle) in state.input_lifecycles() {
                if let Some(category) = lifecycle.category
                    && !categories.iter().any(|candidate| candidate.id == category)
                {
                    let error = fm_model::InputLifecycleError::CategoryInUse {
                        category,
                        input: *input,
                    };
                    return Err(rejection(
                        RejectionCode::Conflict,
                        "categories",
                        error.code(),
                        error.to_string(),
                    ));
                }
            }
            state.set_input_categories_unchecked(categories.clone());
        }
        EngineInputLifecycleEdit::Import { items } => {
            let (inputs, refusals) = added_inputs(state, edit, &[])?;
            let mut staged = state.clone();
            for (input, name) in &inputs {
                let item = items
                    .iter()
                    .find(|item| item.input == *input)
                    .expect("accepted items come from the request");
                staged.add_input(*input, name.clone()).map_err(catalogue)?;
                staged.set_input_source_unchecked(
                    *input,
                    InputSourceSpec::Media {
                        asset_uri: item.asset_uri.clone(),
                    },
                );
                staged.insert_media_input(*input, fm_model::MediaPlaybackSettings::default());
            }
            *state = staged;
            added = inputs;
            refused = refusals;
        }
        EngineInputLifecycleEdit::RemoveInputs { inputs } => {
            if inputs.is_empty() || inputs.len() > MAX_SHOW_INPUTS {
                return Err(rejection(
                    RejectionCode::InvalidCommand,
                    "inputs",
                    "invalid_batch",
                    format!("a batch removal names 1 to {MAX_SHOW_INPUTS} inputs"),
                ));
            }
            let mut staged = state.clone();
            for input in inputs {
                staged.remove_input(*input).map_err(catalogue)?;
            }
            *state = staged;
        }
    }
    Ok(EngineEvent::InputLifecycleChanged {
        edit: edit.clone(),
        added,
        refused,
    })
}

/// The exact edit that reverts `edit` on `state`, or `None` when it has no
/// inverse (batch removal) or targets nothing in `state`.
pub(crate) fn inverse(state: &ShowState, edit: &EngineInputLifecycleEdit) -> Option<EngineCommand> {
    let lifecycle = |input: InputId| {
        state
            .inputs()
            .contains(&input)
            .then(|| state.input_lifecycle(input))
    };
    let edit = match edit {
        EngineInputLifecycleEdit::Duplicate { .. } | EngineInputLifecycleEdit::Import { .. } => {
            let (added, _) = added_inputs(state, edit, &[]).ok()?;
            EngineInputLifecycleEdit::RemoveInputs {
                inputs: added.into_iter().map(|(input, _)| input).collect(),
            }
        }
        EngineInputLifecycleEdit::ReplaceSource { input, .. } => {
            EngineInputLifecycleEdit::ReplaceSource {
                input: *input,
                source: state.input_source(*input)?.clone(),
            }
        }
        EngineInputLifecycleEdit::SetCategory { input, .. } => {
            EngineInputLifecycleEdit::SetCategory {
                input: *input,
                category: lifecycle(*input)?.category,
            }
        }
        EngineInputLifecycleEdit::SetColor { input, .. } => EngineInputLifecycleEdit::SetColor {
            input: *input,
            color: lifecycle(*input)?.color,
        },
        EngineInputLifecycleEdit::SetPaused { input, .. } => EngineInputLifecycleEdit::SetPaused {
            input: *input,
            paused: lifecycle(*input)?.paused,
        },
        EngineInputLifecycleEdit::SetClosed { input, .. } => EngineInputLifecycleEdit::SetClosed {
            input: *input,
            closed: lifecycle(*input)?.closed,
        },
        EngineInputLifecycleEdit::SetCategories { .. } => EngineInputLifecycleEdit::SetCategories {
            categories: state.input_categories().to_vec(),
        },
        EngineInputLifecycleEdit::RemoveInputs { .. } => return None,
    };
    Some(EngineCommand::InputLifecycle(edit))
}

/// Whether removing `inputs` exactly reverses `forward` (a duplicate or an
/// import) on `state`: they are the last inputs of the order, under the
/// names `forward` gives them, and ungraded.
pub(crate) fn removal_reverses(
    state: &ShowState,
    inputs: &[InputId],
    forward: &EngineInputLifecycleEdit,
) -> bool {
    if !matches!(
        forward,
        EngineInputLifecycleEdit::Duplicate { .. } | EngineInputLifecycleEdit::Import { .. }
    ) || inputs.is_empty()
        || state.inputs().len() < inputs.len()
    {
        return false;
    }
    let tail = &state.inputs()[state.inputs().len() - inputs.len()..];
    if tail != inputs
        || inputs
            .iter()
            .any(|input| state.grading().inputs.contains_key(input))
    {
        return false;
    }
    added_inputs(state, forward, inputs).is_ok_and(|(added, _)| {
        added.len() == inputs.len()
            && added
                .iter()
                .all(|(input, name)| state.input_name(*input) == Some(name.as_str()))
    })
}
