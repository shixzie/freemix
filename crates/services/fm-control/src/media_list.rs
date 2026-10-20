//! Wire mapping of media list inputs (IN-006): the `media_list` command,
//! the `media_list_changed` event, and the snapshot's `media_list_state`
//! records.

use fm_engine::{
    Engine, EngineCommand, EngineMediaListCommand, EngineMediaListNavigation,
    EngineMediaListOperation, EngineMediaListStatus,
};
use fm_model::{MediaListItem, MediaListOptions};
use fm_protocol::{
    CommandPayload, EventPayload, MediaListItemSpec, MediaListNavigation, MediaListOperation,
    MediaListOptionsSpec, MediaListStateMessage, MediaListStatus, WireInputId,
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

const fn model_options(options: MediaListOptionsSpec) -> MediaListOptions {
    MediaListOptions {
        auto_next: options.auto_next,
        auto_first: options.auto_first,
        loop_list: options.loop_list,
        shuffle_seed: options.shuffle_seed,
    }
}

const fn wire_options(options: MediaListOptions) -> MediaListOptionsSpec {
    MediaListOptionsSpec {
        auto_next: options.auto_next,
        auto_first: options.auto_first,
        loop_list: options.loop_list,
        shuffle_seed: options.shuffle_seed,
    }
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
        MediaListOperation::Set { items, options } => EngineMediaListOperation::Set {
            items: items.iter().map(model_item).collect(),
            options: model_options(*options),
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
        EngineMediaListOperation::Set { items, options } => MediaListOperation::Set {
            items: items.iter().map(wire_item).collect(),
            options: wire_options(*options),
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

/// The wire form of one list's status.
#[must_use]
pub fn wire_status(status: &EngineMediaListStatus) -> MediaListStatus {
    MediaListStatus {
        input: WireInputId::from_domain(status.input),
        items: status.list.items.iter().map(wire_item).collect(),
        options: wire_options(status.list.options),
        active: status.list.active,
        cycle: status.list.cycle,
        order: status.order.clone(),
        position: u32::try_from(status.position).expect("lists are bounded"),
    }
}

/// How many `media_list_state` records follow a snapshot.
#[must_use]
pub fn media_list_state_count(engine: &Engine) -> u16 {
    u16::try_from(engine.show().media_lists().len()).expect("media lists are bounded")
}

/// The `media_list_state` records after a snapshot, ascending by input.
#[must_use]
pub fn media_list_states(engine: &Engine) -> Vec<MediaListStateMessage> {
    engine
        .media_list_statuses()
        .iter()
        .map(|status| MediaListStateMessage {
            revision: engine.revision().get(),
            status: wire_status(status),
        })
        .collect()
}

/// The event of an accepted media list command; `None` when the list is
/// gone (never for an accepted command).
#[must_use]
pub fn media_list_changed(
    engine: &Engine,
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
        status: Box::new(wire_status(&status)),
    })
}
