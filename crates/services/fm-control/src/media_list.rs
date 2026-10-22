//! Wire mapping of media list inputs (IN-006): the `media_list` command,
//! the `media_list_changed` event, and the snapshot's `media_list_state`
//! records.
//!
//! Each item's file length (Protocol 2.91) is runtime knowledge the media
//! plane reports by asset ([`MediaListLengths`]); the wire status joins it
//! to the engine's list.

use fm_engine::{
    Engine, EngineCommand, EngineMediaListCommand, EngineMediaListNavigation,
    EngineMediaListOperation, EngineMediaListStatus,
};
use fm_model::{MediaListItem, MediaListOptions};
use std::collections::BTreeMap;

use fm_protocol::{
    CommandPayload, EventPayload, MediaListItemLength, MediaListItemSpec, MediaListNavigation,
    MediaListOperation, MediaListOptionsSpec, MediaListStateMessage, MediaListStatus, WireInputId,
};

// The wire and the model agree on every list bound.
const _: () = {
    assert!(fm_protocol::MAX_MEDIA_LIST_ITEMS == fm_model::MAX_MEDIA_LIST_ITEMS);
    assert!(fm_protocol::MAX_MEDIA_LIST_STATES as usize == fm_model::MAX_MEDIA_LISTS);
    assert!(fm_protocol::MAX_MEDIA_LIST_TITLE_BYTES == fm_model::MAX_MEDIA_LIST_TITLE_BYTES);
    assert!(fm_protocol::MAX_MEDIA_LIST_TEXT_BYTES == fm_model::MAX_MEDIA_LIST_TEXT_BYTES);
    assert!(fm_protocol::MAX_MEDIA_LIST_ITEM_ID == fm_model::MAX_MEDIA_LIST_ITEM_ID);
};

fn model_item(item: &MediaListItemSpec) -> MediaListItem {
    MediaListItem {
        id: item.id,
        asset_uri: item.uri.clone(),
        title: item.title.clone(),
        mark_in: item.mark_in,
        mark_out: item.mark_out,
    }
}

fn wire_item(item: &MediaListItem) -> MediaListItemSpec {
    MediaListItemSpec {
        id: item.id,
        uri: item.asset_uri.clone(),
        title: item.title.clone(),
        mark_in: item.mark_in,
        mark_out: item.mark_out,
    }
}

const fn model_transition_kind(
    kind: fm_protocol::MediaListTransitionKind,
) -> fm_model::MediaListTransitionKind {
    match kind {
        fm_protocol::MediaListTransitionKind::Cut => fm_model::MediaListTransitionKind::Cut,
        fm_protocol::MediaListTransitionKind::Fade => fm_model::MediaListTransitionKind::Fade,
        fm_protocol::MediaListTransitionKind::Dip => fm_model::MediaListTransitionKind::Dip,
        fm_protocol::MediaListTransitionKind::Wipe => fm_model::MediaListTransitionKind::Wipe,
    }
}

const fn wire_transition_kind(
    kind: fm_model::MediaListTransitionKind,
) -> fm_protocol::MediaListTransitionKind {
    match kind {
        fm_model::MediaListTransitionKind::Cut => fm_protocol::MediaListTransitionKind::Cut,
        fm_model::MediaListTransitionKind::Fade => fm_protocol::MediaListTransitionKind::Fade,
        fm_model::MediaListTransitionKind::Dip => fm_protocol::MediaListTransitionKind::Dip,
        fm_model::MediaListTransitionKind::Wipe => fm_protocol::MediaListTransitionKind::Wipe,
    }
}

const fn model_options(options: MediaListOptionsSpec) -> MediaListOptions {
    MediaListOptions {
        auto_next: options.auto_next,
        auto_first: options.auto_first,
        loop_list: options.loop_list,
        shuffle_seed: options.shuffle_seed,
        transition: fm_model::MediaListTransition {
            kind: model_transition_kind(options.transition.kind),
            duration_frames: options.transition.duration_frames,
        },
    }
}

const fn wire_options(options: MediaListOptions) -> MediaListOptionsSpec {
    MediaListOptionsSpec {
        auto_next: options.auto_next,
        auto_first: options.auto_first,
        loop_list: options.loop_list,
        shuffle_seed: options.shuffle_seed,
        transition: fm_protocol::MediaListTransitionSpec {
            kind: wire_transition_kind(options.transition.kind),
            duration_frames: options.transition.duration_frames,
        },
    }
}

/// Whether media input `frame.input`'s clip ended on `frame`, or (a media
/// list's) came within its blending transition's frames of its out point
/// ([`transition_lead_reached`]): a list under Auto Next then steps.
pub(crate) fn item_done(
    engine: &Engine,
    frame: &fm_engine::EngineMediaFrame,
    leads: &mut BTreeMap<fm_types::InputId, (u32, u64, bool)>,
) -> bool {
    let ended = frame.tick.as_ref().is_some_and(|tick| {
        tick.events
            .iter()
            .any(|event| matches!(event, fm_engine::fm_playback::MediaEvent::Ended { .. }))
    });
    // Always observed, so the lead knows the frames left on every frame.
    let lead = transition_lead_reached(engine, frame, leads);
    ended || lead
}

/// Whether media list `frame.input`'s active item just came within its
/// blending transition's frames of its out point while playing under Auto
/// Next, with a next item that plays (IN-006): the authority then steps
/// the list early so the two items overlap. `leads` keeps each list's
/// active item, the output frames it had left on its last playing frame,
/// and whether that frame was the item's first observed one; the lead
/// fires once, on the frame the item crosses it, never on an item's first
/// observed frame (a new item's, or the same item's again when its frames
/// left grew, as when a list of one looping item restarts it): an item
/// shorter than its transition (already within it on its first frame)
/// steps on its first played frame after the first, every pass, so
/// the list keeps its length and the item plays on under the next one
/// instead of freezing on its last frame for the whole transition.
pub(crate) fn transition_lead_reached(
    engine: &Engine,
    frame: &fm_engine::EngineMediaFrame,
    leads: &mut BTreeMap<fm_types::InputId, (u32, u64, bool)>,
) -> bool {
    let input = frame.input;
    let Some(list) = engine.show().media_list(input) else {
        leads.remove(&input);
        return false;
    };
    let lead = u64::from(list.options.transition.blend_frames());
    let playing = frame.tick.as_ref().is_some_and(|tick| {
        tick.state == fm_engine::fm_playback::MediaState::Playing && tick.frame.is_some()
    });
    if lead == 0 || !list.options.auto_next || !playing {
        leads.remove(&input);
        return false;
    }
    let order = list.play_order();
    let position = list.position().unwrap_or(0);
    let next_plays = position + 1 < order.len() || list.options.loop_list;
    let (Some(tick), Some(status)) = (frame.tick.as_ref(), engine.media_status(input)) else {
        return false;
    };
    let Some(presented) = tick.frame else {
        return false;
    };
    let remaining = remaining_output_frames(
        status
            .playable_end
            .saturating_sub(presented.get().saturating_add(1)),
        status.source_rate.unwrap_or_else(|| engine.frame_rate()),
        engine.frame_rate(),
        status.settings.speed_basis_points,
    );
    // A new item, or the same item starting another pass (a list of one
    // looping item, or the item played again): its frames left grew.
    let first = leads
        .get(&input)
        .is_none_or(|&(active, before, _)| active != list.active || remaining > before);
    let previous = leads.insert(input, (list.active, remaining, first));
    next_plays
        && remaining <= lead
        && previous.is_some_and(|(active, before, was_first)| {
            active == list.active && (before > lead || was_first)
        })
}

/// How many output frames `source_frames` source frames at `source_rate`
/// last at `output_rate` and `speed_basis_points` (rounded down).
fn remaining_output_frames(
    source_frames: u64,
    source_rate: fm_types::FrameRate,
    output_rate: fm_types::FrameRate,
    speed_basis_points: u32,
) -> u64 {
    let numerator = u128::from(source_frames)
        * u128::from(source_rate.denominator())
        * u128::from(output_rate.numerator())
        * 10_000;
    let denominator = u128::from(source_rate.numerator())
        * u128::from(output_rate.denominator())
        * u128::from(speed_basis_points.max(1));
    u64::try_from(numerator / denominator.max(1)).unwrap_or(u64::MAX)
}

const fn model_navigation(navigation: MediaListNavigation) -> EngineMediaListNavigation {
    match navigation {
        MediaListNavigation::Next => EngineMediaListNavigation::Next,
        MediaListNavigation::Previous => EngineMediaListNavigation::Previous,
        MediaListNavigation::First => EngineMediaListNavigation::First,
        MediaListNavigation::Last => EngineMediaListNavigation::Last,
        MediaListNavigation::Goto(position) => EngineMediaListNavigation::Goto(position),
        MediaListNavigation::Item(item) => EngineMediaListNavigation::Item(item),
    }
}

const fn wire_navigation(navigation: EngineMediaListNavigation) -> MediaListNavigation {
    match navigation {
        EngineMediaListNavigation::Next => MediaListNavigation::Next,
        EngineMediaListNavigation::Previous => MediaListNavigation::Previous,
        EngineMediaListNavigation::First => MediaListNavigation::First,
        EngineMediaListNavigation::Last => MediaListNavigation::Last,
        EngineMediaListNavigation::Goto(position) => MediaListNavigation::Goto(position),
        EngineMediaListNavigation::Item(item) => MediaListNavigation::Item(item),
    }
}

/// Maps a `media_list` command.
#[must_use]
pub fn media_list_engine_command(payload: &CommandPayload) -> EngineCommand {
    let CommandPayload::MediaList { input, operation } = payload else {
        unreachable!("only media list commands are delegated");
    };
    let operation = match operation {
        MediaListOperation::Create {
            name,
            items,
            options,
        } => EngineMediaListOperation::Create {
            name: name.clone(),
            items: items.iter().map(model_item).collect(),
            options: model_options(*options),
        },
        MediaListOperation::Set {
            items,
            options,
            active,
        } => EngineMediaListOperation::Set {
            items: items.iter().map(model_item).collect(),
            options: model_options(*options),
            active: *active,
        },
        MediaListOperation::Navigate(navigation) => {
            EngineMediaListOperation::Navigate(model_navigation(*navigation))
        }
        MediaListOperation::Advance { from } => EngineMediaListOperation::Advance { from: *from },
    };
    EngineCommand::MediaList(EngineMediaListCommand {
        input: input.to_domain(),
        operation,
    })
}

/// The wire form of a media list command (for undo and redo previews).
#[must_use]
pub fn wire_payload(command: &EngineMediaListCommand) -> CommandPayload {
    let operation = match &command.operation {
        EngineMediaListOperation::Create {
            name,
            items,
            options,
        } => MediaListOperation::Create {
            name: name.clone(),
            items: items.iter().map(wire_item).collect(),
            options: wire_options(*options),
        },
        EngineMediaListOperation::Set {
            items,
            options,
            active,
        } => MediaListOperation::Set {
            items: items.iter().map(wire_item).collect(),
            options: wire_options(*options),
            active: *active,
        },
        EngineMediaListOperation::Navigate(navigation) => {
            MediaListOperation::Navigate(wire_navigation(*navigation))
        }
        EngineMediaListOperation::Advance { from } => MediaListOperation::Advance { from: *from },
    };
    CommandPayload::MediaList {
        input: WireInputId::from_domain(command.input),
        operation,
    }
}

/// Each media list item file's length as the media plane last probed it,
/// by asset URI (Protocol 2.91). A file absent here is reported unknown.
pub type MediaListLengths = BTreeMap<String, MediaListItemLength>;

/// The wire form of one list's status, with each item's probed length
/// from `lengths`. A list whose lengths would not fit its record reports
/// them unknown (see [`fm_protocol::media_list_status_fits`]).
///
/// # Panics
///
/// Never: a list's position is bounded by its at most 256 items.
#[must_use]
pub fn wire_status(status: &EngineMediaListStatus, lengths: &MediaListLengths) -> MediaListStatus {
    let mut wire = MediaListStatus {
        input: WireInputId::from_domain(status.input),
        items: status.list.items.iter().map(wire_item).collect(),
        options: wire_options(status.list.options),
        active: status.list.active,
        cycle: status.list.cycle,
        order: status.order.clone(),
        position: u32::try_from(status.position).expect("lists are bounded"),
        lengths: status
            .list
            .items
            .iter()
            .map(|item| {
                lengths
                    .get(&item.asset_uri)
                    .copied()
                    .filter(|length| length.is_valid())
            })
            .collect(),
    };
    if wire.lengths.iter().any(Option::is_some) && !fm_protocol::media_list_status_fits(&wire) {
        wire.lengths.fill(None);
    }
    wire
}

/// How many `media_list_state` records follow a snapshot.
#[must_use]
pub fn media_list_state_count(engine: &Engine) -> u16 {
    u16::try_from(engine.show().media_lists().len()).expect("media lists are bounded")
}

/// The `media_list_state` records after a snapshot, ascending by input.
#[must_use]
pub fn media_list_states(
    engine: &Engine,
    lengths: &MediaListLengths,
) -> Vec<MediaListStateMessage> {
    engine
        .media_list_statuses()
        .iter()
        .map(|status| MediaListStateMessage {
            revision: engine.revision().get(),
            status: wire_status(status, lengths),
        })
        .collect()
}

/// Every list's wire status, by input.
#[must_use]
pub fn wire_statuses(
    engine: &Engine,
    lengths: &MediaListLengths,
) -> BTreeMap<fm_types::InputId, MediaListStatus> {
    engine
        .media_list_statuses()
        .iter()
        .map(|status| (status.input, wire_status(status, lengths)))
        .collect()
}

/// Every media list item's asset URI, once each.
#[must_use]
pub fn item_assets(engine: &Engine) -> std::collections::BTreeSet<String> {
    engine
        .show()
        .media_lists()
        .values()
        .flat_map(|list| list.items.iter().map(|item| item.asset_uri.clone()))
        .collect()
}

/// The event of an accepted media list command; `None` when the list is
/// gone (never for an accepted command).
#[must_use]
pub fn media_list_changed(
    engine: &Engine,
    lengths: &MediaListLengths,
    input: fm_types::InputId,
    frame: u64,
    operation: &EngineMediaListOperation,
) -> Option<EventPayload> {
    let status = engine.media_list_status(input)?;
    let created = match operation {
        EngineMediaListOperation::Create { name, .. } => Some(name.clone()),
        _ => None,
    };
    Some(EventPayload::MediaListChanged {
        frame,
        operation: operation.token(),
        created,
        status: Box::new(wire_status(&status, lengths)),
    })
}
