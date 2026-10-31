//! IN-006: a cross-input bank addresses each input's media-list player.

use std::num::NonZeroU128;

use fm_playback::{
    ClipId, ClipLibrary, FixtureClip, GoId, GoScheduleOutcome, ListAction, ListGo, ListGoBank,
    ListGoBankError, ListItem, ListItemId, ListOptions, ListPlayer, MAX_GO_BANK_INPUTS, MediaList,
    ScheduleCoordinate,
};
use fm_types::InputId;

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn player() -> ListPlayer<u8> {
    let mut library = ClipLibrary::default();
    library
        .insert(
            FixtureClip::new(ClipId::new(1), vec![10, 11])
                .unwrap()
                .into(),
        )
        .unwrap();
    let list = MediaList::new(
        vec![ListItem::new(ListItemId::new(1), ClipId::new(1))],
        ListOptions {
            auto_next: true,
            ..ListOptions::default()
        },
    )
    .unwrap();
    ListPlayer::new(library, list).unwrap()
}

fn play() -> ListGo {
    ListGo {
        id: GoId::new(1),
        coordinate: ScheduleCoordinate::new(0),
        action: ListAction::Play(ListItemId::new(1)),
    }
}

#[test]
fn a_bank_addresses_each_inputs_player() {
    let mut bank = ListGoBank::new();
    bank.insert(input(1), player()).unwrap();
    bank.insert(input(2), player()).unwrap();
    assert_eq!(bank.len(), 2);
    assert!(!bank.is_empty());
    assert_eq!(bank.inputs().collect::<Vec<_>>(), vec![input(1), input(2)]);
    assert_eq!(
        bank.insert(input(1), player()),
        Err(ListGoBankError::DuplicateInput(input(1)))
    );
    assert_eq!(
        bank.schedule_go(input(2), play()),
        Ok(GoScheduleOutcome::Scheduled)
    );
    assert_eq!(bank.player(input(1)).unwrap().pending_go_count(), 0);
    assert_eq!(bank.player(input(2)).unwrap().pending_go_count(), 1);
    assert_eq!(
        bank.schedule_go(input(3), play()),
        Err(ListGoBankError::UnknownInput(input(3)))
    );
}

#[test]
fn a_bank_is_bounded_and_removes_players() {
    let mut bank = ListGoBank::new();
    for value in 1..=u128::try_from(MAX_GO_BANK_INPUTS).unwrap() {
        bank.insert(input(value), player()).unwrap();
    }
    assert_eq!(
        bank.insert(input(999), player()),
        Err(ListGoBankError::TooManyInputs {
            maximum: MAX_GO_BANK_INPUTS
        })
    );
    assert!(bank.remove(input(1)).is_some());
    bank.insert(input(999), player()).unwrap();
    assert_eq!(bank.len(), MAX_GO_BANK_INPUTS);
}
