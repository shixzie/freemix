//! Media List inputs (IN-006).
//!
//! A list input is a media input ([`crate::media`]) plus one
//! [`fm_model::MediaList`] in the show. Activating an item points the
//! input's source at the item's file and its trim at the item's marks; the
//! input's one media transport then plays it. Nothing else plays a list:
//! the media plane opens the new file exactly as it opens a replaced
//! source, and the transport is cued (or started) on it.
//!
//! Commands:
//!
//! - `Create` appends a new list input (a project edit, undone by removing
//!   the input);
//! - `Set` replaces the items (order, files, titles, marks) and options (a
//!   padlocked, undoable project edit);
//! - `Navigate` steps through the play order (next, previous, first, last,
//!   go to position N, or go to an item): a live operation;
//! - `Advance` is what the authority journals when the active item's clip
//!   ends under Auto Next: the next item plays, or at the end of the list
//!   the list loops (reshuffled for the new cycle) or, with Auto First, the
//!   first item is cued stopped. It names the item that ended, so a stale
//!   advance (the operator moved on) is refused.
//!
//! Every step is planned once against the pre-command show and transport
//! ([`plan`]) and is a pure function of them, so a journal replay reaches
//! the same items on the same frames. Whether the next item plays follows
//! the transport: a playing (or waiting) list keeps playing, a stopped or
//! paused one cues.

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_model::{MediaList, MediaListItem, MediaListOptions};
use fm_types::InputId;

use crate::ShowState;

/// One step through a list's play order. Positions are zero-based in play
/// order, like a paged input's pages.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EngineMediaListNavigation {
    /// The following item; past the last one only with loop.
    Next,
    /// The preceding item; before the first one only with loop.
    Previous,
    First,
    Last,
    /// Play-order position `n`, clamped to the last item.
    Goto(u32),
    /// The item with this id.
    Item(u32),
}

/// One media list command's operation.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum EngineMediaListOperation {
    /// Appends a new media list input named `name`, on its first item in
    /// play order, stopped.
    Create {
        name: String,
        items: Vec<MediaListItem>,
        options: MediaListOptions,
    },
    /// Replaces the items and options. `Some(active)`, one of the new
    /// items, becomes the active item (an undo restores the item a `Set`
    /// dropped); with `None` the active item stays active when it is kept
    /// (its new marks apply at once), otherwise the first item in play order
    /// is cued. A newly active item is cued, stopped.
    Set {
        items: Vec<MediaListItem>,
        options: MediaListOptions,
        active: Option<u32>,
    },
    Navigate(EngineMediaListNavigation),
    /// The active item `from` ended under Auto Next.
    Advance {
        from: u32,
    },
}

impl EngineMediaListOperation {
    /// Navigation and advances are live operator controls; the others edit
    /// the project.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        matches!(self, Self::Navigate(_) | Self::Advance { .. })
    }

    /// The operation's wire token.
    #[must_use]
    pub const fn token(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Set { .. } => "set",
            Self::Navigate(_) => "navigate",
            Self::Advance { .. } => "advance",
        }
    }
}

/// A media list command for one input.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EngineMediaListCommand {
    pub input: InputId,
    pub operation: EngineMediaListOperation,
}

/// An item becoming active.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Activation {
    pub(crate) item: MediaListItem,
    /// Whether the item starts playing (else it is cued, stopped).
    pub(crate) play: bool,
    /// Whether the input already plays the item's file: its transport is
    /// re-cued in place instead of reopened.
    pub(crate) same_source: bool,
}

/// What an accepted list command does, planned against the pre-command
/// show and transport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MediaListPlan {
    pub(crate) input: InputId,
    /// The list afterwards.
    pub(crate) list: MediaList,
    pub(crate) activation: Option<Activation>,
    /// The marks of the active item changed while it stays active: its
    /// open transport takes the new trim.
    pub(crate) retrim: bool,
}

fn rejection(code: RejectionCode, field: &str, issue: &str, message: String) -> Rejection {
    Rejection::new(code, message.clone()).with_field(FieldIssue::new(field, issue, message))
}

fn list_rejection(error: &fm_model::MediaListError) -> Rejection {
    rejection(
        RejectionCode::InvalidCommand,
        "items",
        error.code(),
        error.to_string(),
    )
}

fn not_a_list(state: &ShowState, input: InputId) -> Rejection {
    if state.inputs().contains(&input) {
        rejection(
            RejectionCode::InvalidCommand,
            "input",
            "not_media_list",
            format!("input {input} is not a media list input"),
        )
    } else {
        rejection(
            RejectionCode::NotFound,
            "input",
            "unknown_input",
            format!("input {input} is not part of this show"),
        )
        .with_reason(crate::error::unknown_input_reason(input))
    }
}

fn end_of_list(input: InputId) -> Rejection {
    rejection(
        RejectionCode::Conflict,
        "input",
        "end_of_list",
        format!("media list {input} has no item after its last one"),
    )
}

/// The current asset of `input`, when it is a media input.
fn current_asset(state: &ShowState, input: InputId) -> Option<&str> {
    match state.input_source(input) {
        Some(fm_model::InputSourceSpec::Media { asset_uri }) => Some(asset_uri.as_str()),
        _ => None,
    }
}

fn activation(state: &ShowState, input: InputId, item: &MediaListItem, play: bool) -> Activation {
    Activation {
        item: item.clone(),
        play,
        same_source: current_asset(state, input) == Some(item.asset_uri.as_str()),
    }
}

/// Refuses another media list once the show holds
/// [`fm_model::MAX_MEDIA_LISTS`]: a new list or a list input's duplicate.
pub(crate) fn check_room(state: &ShowState) -> Result<(), Rejection> {
    if state.media_lists().len() >= fm_model::MAX_MEDIA_LISTS {
        return Err(rejection(
            RejectionCode::Conflict,
            "input",
            "too_many_media_lists",
            format!(
                "a show holds at most {} media lists",
                fm_model::MAX_MEDIA_LISTS
            ),
        ));
    }
    Ok(())
}

/// Plans `command` against `state`; `playing` says whether the list's
/// transport plays (or waits for data, or is opening to play).
///
/// # Errors
///
/// Refuses an unknown input, an input that is not a list (or, for create,
/// one that already exists), invalid items, an unknown item, and a stale or
/// final advance.
#[allow(clippy::too_many_lines)] // one arm per operation
pub(crate) fn plan(
    state: &ShowState,
    command: &EngineMediaListCommand,
    playing: bool,
) -> Result<MediaListPlan, Rejection> {
    let input = command.input;
    if let EngineMediaListOperation::Create { items, options, .. } = &command.operation {
        if state.inputs().contains(&input) {
            return Err(rejection(
                RejectionCode::Conflict,
                "input",
                "duplicate_input",
                format!("input {input} already exists"),
            ));
        }
        check_room(state)?;
        let list =
            MediaList::new(items.clone(), *options).map_err(|error| list_rejection(&error))?;
        let item = list
            .active_item()
            .expect("a new list has its first item")
            .clone();
        return Ok(MediaListPlan {
            input,
            activation: Some(Activation {
                item,
                play: false,
                same_source: false,
            }),
            list,
            retrim: false,
        });
    }
    let current = state
        .media_list(input)
        .ok_or_else(|| not_a_list(state, input))?;
    let mut list = current.clone();
    match &command.operation {
        EngineMediaListOperation::Create { .. } => unreachable!("planned above"),
        EngineMediaListOperation::Set {
            items,
            options,
            active,
        } => {
            fm_model::validate_media_list_items(items).map_err(|error| list_rejection(&error))?;
            list.items.clone_from(items);
            list.options = *options;
            if let Some(active) = *active {
                if list.item(active).is_none() {
                    return Err(rejection(
                        RejectionCode::NotFound,
                        "active",
                        "unknown_item",
                        format!("media list {input} would have no item {active}"),
                    ));
                }
                list.active = active;
            }
            if let Some(item) = list.item(list.active).cloned() {
                // A newly named active item is activated (cued in place
                // when the input already plays its file).
                let before = current
                    .active_item()
                    .filter(|before| before.id == list.active);
                let retrim = before.is_none_or(|before| {
                    before.mark_in != item.mark_in || before.mark_out != item.mark_out
                });
                let moved = before.is_none_or(|before| before.asset_uri != item.asset_uri);
                Ok(MediaListPlan {
                    input,
                    activation: moved.then(|| activation(state, input, &item, false)),
                    list,
                    retrim: retrim && !moved,
                })
            } else {
                let first = list.play_order()[0];
                list.active = first;
                let item = list.item(first).expect("in play order").clone();
                Ok(MediaListPlan {
                    input,
                    activation: Some(activation(state, input, &item, false)),
                    list,
                    retrim: false,
                })
            }
        }
        EngineMediaListOperation::Navigate(navigation) => {
            let target = match navigate(&mut list, *navigation) {
                Step::To(id) => Some(id),
                Step::Stay => None,
                Step::Unknown => {
                    return Err(rejection(
                        RejectionCode::NotFound,
                        "item",
                        "unknown_item",
                        format!("media list {input} has no such item"),
                    ));
                }
            };
            let activation = target.map(|id| {
                let item = list.item(id).expect("navigation lands on an item").clone();
                list.active = id;
                activation(state, input, &item, playing)
            });
            Ok(MediaListPlan {
                input,
                list,
                activation,
                retrim: false,
            })
        }
        EngineMediaListOperation::Advance { from } => {
            if list.active != *from {
                return Err(rejection(
                    RejectionCode::Conflict,
                    "from",
                    "stale_advance",
                    format!(
                        "media list {input} plays item {}, not item {from}",
                        list.active
                    ),
                ));
            }
            if !list.options.auto_next {
                return Err(rejection(
                    RejectionCode::Conflict,
                    "input",
                    "auto_next_off",
                    format!("media list {input} does not advance on its own"),
                ));
            }
            let order = list.play_order();
            let position = list.position().unwrap_or(0);
            let (target, play) = if position + 1 < order.len() {
                (order[position + 1], true)
            } else if list.options.loop_list {
                list.cycle = list.cycle.wrapping_add(1);
                (list.play_order()[0], true)
            } else if list.options.auto_first {
                (order[0], false)
            } else {
                return Err(end_of_list(input));
            };
            let item = list.item(target).expect("in play order").clone();
            list.active = target;
            Ok(MediaListPlan {
                input,
                activation: Some(activation(state, input, &item, play)),
                list,
                retrim: false,
            })
        }
    }
}

/// Where a navigation lands.
enum Step {
    /// The item to activate.
    To(u32),
    /// The list stays where it is (Next at the end, Previous at the start,
    /// without loop).
    Stay,
    /// The navigation names an unknown item.
    Unknown,
}

/// Applies `navigation` to `list`'s cycle.
fn navigate(list: &mut MediaList, navigation: EngineMediaListNavigation) -> Step {
    let order = list.play_order();
    let end = order.len() - 1;
    let position = list.position().unwrap_or(0);
    Step::To(match navigation {
        EngineMediaListNavigation::Next if position < end => order[position + 1],
        EngineMediaListNavigation::Next if list.options.loop_list => {
            list.cycle = list.cycle.wrapping_add(1);
            list.play_order()[0]
        }
        EngineMediaListNavigation::Previous if position > 0 => order[position - 1],
        EngineMediaListNavigation::Previous if list.options.loop_list => order[end],
        EngineMediaListNavigation::Next | EngineMediaListNavigation::Previous => {
            return Step::Stay;
        }
        EngineMediaListNavigation::First => order[0],
        EngineMediaListNavigation::Last => order[end],
        EngineMediaListNavigation::Goto(n) => {
            order[usize::try_from(n).unwrap_or(usize::MAX).min(end)]
        }
        EngineMediaListNavigation::Item(id) => {
            if list.item(id).is_none() {
                return Step::Unknown;
            }
            id
        }
    })
}

/// The settings that play `item`: `settings` with the item's marks as its
/// trim, cued at the in mark.
pub(crate) fn item_settings(
    settings: fm_model::MediaPlaybackSettings,
    item: &MediaListItem,
) -> fm_model::MediaPlaybackSettings {
    let mut settings = settings;
    settings.trim_in = item.mark_in;
    settings.trim_out = item.mark_out;
    settings.start_frame = item.mark_in;
    settings
}

/// Installs `plan` (a list other than a new one) into `state`.
pub(crate) fn apply(state: &mut ShowState, plan: &MediaListPlan) {
    let input = plan.input;
    let active = plan.list.active_item().cloned();
    state.insert_media_list(input, plan.list.clone());
    if let Some(activation) = &plan.activation {
        state.set_input_source_unchecked(
            input,
            fm_model::InputSourceSpec::Media {
                asset_uri: activation.item.asset_uri.clone(),
            },
        );
    }
    if (plan.activation.is_some() || plan.retrim)
        && let (Some(item), Some(settings)) = (active, state.media_settings(input).copied())
    {
        state.set_media_settings(input, item_settings(settings, &item));
    }
}

/// The media operations that re-cue an open transport for `plan`: the new
/// trim, and for an activation of the file already open, a seek to the in
/// mark and play or stop.
pub(crate) fn transport_operations(plan: &MediaListPlan) -> Vec<crate::EngineMediaOperation> {
    let Some(item) = plan.list.active_item() else {
        return Vec::new();
    };
    let trim = crate::EngineMediaOperation::Trim {
        trim_in: item.mark_in,
        trim_out: item.mark_out,
    };
    match &plan.activation {
        Some(activation) if activation.same_source => {
            if activation.play {
                vec![
                    trim,
                    crate::EngineMediaOperation::Seek(crate::EngineMediaSeek::Frame(item.mark_in)),
                    crate::EngineMediaOperation::Play,
                ]
            } else {
                vec![trim, crate::EngineMediaOperation::Stop]
            }
        }
        None if plan.retrim => vec![trim],
        Some(_) | None => Vec::new(),
    }
}

/// Keeps a list's active item marks equal to its input's trim after a
/// media trim command (the transport bar's trim handles mark the item).
pub(crate) fn sync_marks(state: &mut ShowState, input: InputId) {
    let (Some(list), Some(settings)) = (state.media_list(input), state.media_settings(input))
    else {
        return;
    };
    let mut list = list.clone();
    let active = list.active;
    if let Some(item) = list.items.iter_mut().find(|item| item.id == active) {
        item.mark_in = settings.trim_in;
        item.mark_out = settings.trim_out;
    }
    state.insert_media_list(input, list);
}

/// The edit that reverts project edit `command` on `state`: `Set` restores
/// the items, options, and active item; a `Create` is reverted by removing
/// its input.
pub(crate) fn inverse(
    state: &ShowState,
    command: &EngineMediaListCommand,
) -> Option<crate::EngineCommand> {
    match &command.operation {
        EngineMediaListOperation::Create { .. } => (!state.inputs().contains(&command.input))
            .then_some(crate::EngineCommand::RemoveInput {
                input: command.input,
            }),
        EngineMediaListOperation::Set { .. } => {
            let list = state.media_list(command.input)?;
            Some(crate::EngineCommand::MediaList(EngineMediaListCommand {
                input: command.input,
                operation: EngineMediaListOperation::Set {
                    items: list.items.clone(),
                    options: list.options,
                    active: Some(list.active),
                },
            }))
        }
        EngineMediaListOperation::Navigate(_) | EngineMediaListOperation::Advance { .. } => None,
    }
}

/// Whether `a` and `b` are `Set`s of the same input's items and options,
/// whatever item each leaves active.
pub(crate) fn same_edit(a: &EngineMediaListCommand, b: &EngineMediaListCommand) -> bool {
    match (&a.operation, &b.operation) {
        (
            EngineMediaListOperation::Set {
                items: a_items,
                options: a_options,
                ..
            },
            EngineMediaListOperation::Set {
                items: b_items,
                options: b_options,
                ..
            },
        ) => a.input == b.input && a_items == b_items && a_options == b_options,
        _ => false,
    }
}

/// A media list input's replicated state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineMediaListStatus {
    pub input: InputId,
    pub list: MediaList,
    /// Item ids in the current cycle's play order.
    pub order: Vec<u32>,
    /// The active item's zero-based position in `order`.
    pub position: usize,
}
