//! The authority automation needs: the command classes of the show actions
//! a script run or a cue list performs (RC-011, RC-017).
//!
//! A script and a cue only ever ask for ordinary show actions (the
//! [`CueAction`] set), and each of those needs exactly the class the
//! equivalent direct command needs. Starting automation therefore needs,
//! beyond its own `transition` class, the class of every action it will
//! perform, so a session can never use a script or a cue to do what it
//! could not do directly:
//!
//! - `script_run` needs the class of every action the run asks for. The
//!   run is planned on a copy of the show exactly as the command plans it,
//!   so the classes follow the branches the script takes on the current
//!   show.
//! - `cue_go` needs the class of every action of the cue it fires, waits
//!   included, and of every cue its auto-follows reach: the whole chain the
//!   GO arms. The daemon's clock later runs the waits' remaining actions and
//!   the follows (`cue_advance`) on the authority checked here, up front,
//!   when the chain was armed; nothing it can run was not authorized then.
//! - An edit of a list whose cue is waiting changes what the armed chain
//!   will run, so it also needs every class the edited chain runs that the
//!   chain armed before the edit did not (an undo or redo of such an edit
//!   too).
//!
//! The other cue controls (standby, back, skip, stop, reset) never perform
//! an action and need only `transition`.

use fm_auth::CommandClass;
use fm_engine::Engine;
use fm_model::{CueAction, cue_chain_actions};
use fm_protocol::{CommandPayload, CueOperation};

/// Every command class, in the order a [`ClassSet`] lists them.
pub const CLASSES: [CommandClass; 6] = [
    CommandClass::ViewStatus,
    CommandClass::SelectPreview,
    CommandClass::Transition,
    CommandClass::ControlAudio,
    CommandClass::EditProject,
    CommandClass::ManageUsers,
];

const fn bit(class: CommandClass) -> u8 {
    1 << match class {
        CommandClass::ViewStatus => 0,
        CommandClass::SelectPreview => 1,
        CommandClass::Transition => 2,
        CommandClass::ControlAudio => 3,
        CommandClass::EditProject => 4,
        CommandClass::ManageUsers => 5,
    }
}

/// A set of command classes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClassSet(u8);

impl ClassSet {
    pub const fn insert(&mut self, class: CommandClass) {
        self.0 |= bit(class);
    }

    pub fn extend(&mut self, classes: impl IntoIterator<Item = CommandClass>) {
        for class in classes {
            self.insert(class);
        }
    }

    pub const fn union(&mut self, other: Self) {
        self.0 |= other.0;
    }

    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    #[must_use]
    pub const fn contains(self, class: CommandClass) -> bool {
        self.0 & bit(class) != 0
    }

    /// The classes in [`CLASSES`] order.
    pub fn iter(self) -> impl Iterator<Item = CommandClass> {
        CLASSES
            .into_iter()
            .filter(move |class| self.contains(*class))
    }
}

impl FromIterator<CommandClass> for ClassSet {
    fn from_iter<I: IntoIterator<Item = CommandClass>>(classes: I) -> Self {
        let mut set = Self::default();
        set.extend(classes);
        set
    }
}

/// The class the direct command equivalent to `action` needs; `None` for a
/// wait or delay, which performs nothing.
#[must_use]
pub const fn cue_action_class(action: &CueAction) -> Option<CommandClass> {
    Some(match action {
        // `select_preview`.
        CueAction::Preview { .. } => CommandClass::SelectPreview,
        // Takes, overlays, title takes and off-airs, Fade to Black, the
        // playlist, recorders, streams, and media transport are all
        // `transition`-class switcher commands.
        CueAction::Take { .. }
        | CueAction::OverlayOn { .. }
        | CueAction::OverlayOff { .. }
        | CueAction::TitleOn { .. }
        | CueAction::TitleOff { .. }
        | CueAction::FadeToBlack { .. }
        | CueAction::PlaylistStart
        | CueAction::PlaylistStop
        | CueAction::Record { .. }
        | CueAction::Stream { .. }
        | CueAction::Media { .. } => CommandClass::Transition,
        // Saving a title's field values (`title_fields_set`) edits project
        // content.
        CueAction::TitleText { .. } => CommandClass::EditProject,
        // Strip gain and mute (`input_audio_strip_set`).
        CueAction::AudioGain { .. } | CueAction::AudioMute { .. } => CommandClass::ControlAudio,
        CueAction::Wait { .. } | CueAction::Delay { .. } => return None,
    })
}

fn classes<'a>(actions: impl IntoIterator<Item = &'a CueAction>) -> ClassSet {
    actions.into_iter().filter_map(cue_action_class).collect()
}

/// The classes of the show actions `payload` performs or arms on `engine`
/// now, beyond its own class (see the module documentation). Empty for a
/// command that runs no automation, and for one the engine will refuse
/// before anything runs (an unknown list, script, or cue).
pub fn action_classes(engine: &Engine, payload: &CommandPayload) -> ClassSet {
    match payload {
        CommandPayload::RunScript { script } => classes(&engine.script_run_actions(*script)),
        CommandPayload::CueControl {
            list,
            operation: CueOperation::Go { cue },
        } => engine
            .show()
            .cue_list(*list)
            .and_then(|found| cue.or(found.state.standby).map(|cue| (found, cue)))
            .map_or_else(ClassSet::default, |(found, cue)| {
                classes(cue_chain_actions(&found.cues, cue, 0))
            }),
        CommandPayload::SetCueList { list, spec } => {
            let Some(found) = engine.show().cue_list(*list) else {
                return ClassSet::default();
            };
            let Some(pending) = found.state.pending else {
                return ClassSet::default();
            };
            let armed = classes(cue_chain_actions(&found.cues, pending.cue, pending.action));
            let edited: Vec<fm_model::Cue> = spec.cues.iter().map(crate::cue::domain_cue).collect();
            classes(cue_chain_actions(&edited, pending.cue, pending.action)).without(armed)
        }
        _ => ClassSet::default(),
    }
}
