//! The console's selected input (RC-004).
//!
//! A shortcut or cue action may target "the selected input": the input
//! selected on the console that fires it. Studio selects the input tile the
//! operator last clicked (or right-clicked, or reached with the keyboard).
//! The selection is local to this console, never sent on its own: when the
//! console fires a shortcut or sends a cue GO or arm, the selected input
//! travels with the command as its `selected` binding, and the authority
//! resolves the action's target to it there.

use fm_protocol::WireInputId;
use fm_types::InputId;

use crate::{CueIntent, ShortcutIntent, StudioIntent};

fn key() -> egui::Id {
    egui::Id::new("studio-selected-input")
}

/// Publishes this frame's selection to the drawing code that previews what
/// an action on the selected input does.
pub(crate) fn publish(context: &egui::Context, selected: Option<InputId>) {
    context.data_mut(|data| match selected {
        Some(input) => {
            data.insert_temp(key(), input);
        }
        None => data.remove::<InputId>(key()),
    });
}

/// The selection this frame's drawing code reads (see [`publish`]).
#[must_use]
pub(crate) fn selected_input(context: &egui::Context) -> Option<InputId> {
    context.data(|data| data.get_temp::<InputId>(key()))
}

/// Gives every fire this frame's intents ask for the console's selected
/// input: a shortcut fire, and a cue GO or arm, that does not already name
/// one.
pub(crate) fn bind(selected: Option<InputId>, intents: &mut [StudioIntent]) {
    let Some(selected) = selected.map(WireInputId::from_domain) else {
        return;
    };
    for intent in intents {
        let (StudioIntent::Shortcut(ShortcutIntent::Fire { targets, .. })
        | StudioIntent::Cue(CueIntent::Control {
            operation:
                fm_protocol::CueOperation::Go { targets, .. }
                | fm_protocol::CueOperation::Arm { targets, .. },
            ..
        })) = intent
        else {
            continue;
        };
        targets.selected.get_or_insert(selected);
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_protocol::{CueOperation, WireTargetBindings};

    use super::*;

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    #[test]
    fn fires_carry_the_selected_input_and_nothing_else_changes() {
        let mut intents = vec![
            StudioIntent::Shortcut(ShortcutIntent::Fire {
                shortcut: 1,
                targets: WireTargetBindings::default(),
            }),
            StudioIntent::Cue(CueIntent::Control {
                list: 1,
                operation: CueOperation::Go {
                    cue: None,
                    targets: WireTargetBindings::default(),
                },
            }),
            StudioIntent::Cue(CueIntent::Control {
                list: 1,
                operation: CueOperation::Arm {
                    cue: Some(2),
                    in_ms: 1_000,
                    targets: WireTargetBindings::default(),
                },
            }),
            StudioIntent::Cue(CueIntent::Control {
                list: 1,
                operation: CueOperation::Stop,
            }),
            StudioIntent::Shortcut(ShortcutIntent::Remove { shortcut: 2 }),
        ];
        let untouched = intents[3..].to_vec();
        bind(Some(input(7)), &mut intents);
        let selected = Some(WireInputId::from_domain(input(7)));
        for intent in &intents[..3] {
            let targets = match intent {
                StudioIntent::Shortcut(ShortcutIntent::Fire { targets, .. })
                | StudioIntent::Cue(CueIntent::Control {
                    operation: CueOperation::Go { targets, .. } | CueOperation::Arm { targets, .. },
                    ..
                }) => targets,
                other => panic!("unexpected {other:?}"),
            };
            assert_eq!(targets.selected, selected);
            assert_eq!(targets.preview, None, "the authority binds Preview");
            assert_eq!(targets.program, None, "the authority binds Program");
        }
        assert_eq!(intents[3..], untouched[..]);
    }

    #[test]
    fn no_selection_binds_nothing() {
        let mut intents = vec![StudioIntent::Shortcut(ShortcutIntent::Fire {
            shortcut: 1,
            targets: WireTargetBindings::default(),
        })];
        let before = intents.clone();
        bind(None, &mut intents);
        assert_eq!(intents, before);
    }

    #[test]
    fn the_published_selection_reads_back() {
        let context = egui::Context::default();
        assert_eq!(selected_input(&context), None);
        publish(&context, Some(input(3)));
        assert_eq!(selected_input(&context), Some(input(3)));
        publish(&context, None);
        assert_eq!(selected_input(&context), None);
    }
}
