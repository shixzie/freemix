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
//!   show. A script that waits (Protocol 2.62) runs its later parts on
//!   later frames, on a show not known now, so its run also needs the class
//!   of every action verb its source holds ([`fm_script::Program::verbs`]),
//!   up front: the clock's `cue_advance` of list 0 later resumes it on the
//!   authority checked here, and its script cannot change while it runs.
//!   `script_stop` performs nothing and needs only `transition`.
//! - `cue_go` needs the class of every action of the cue it fires, waits
//!   included, and of every cue its auto-follows reach: the whole chain the
//!   GO arms. The daemon's clock later runs the waits' remaining actions and
//!   the follows (`cue_advance`) on the authority checked here, up front,
//!   when the chain was armed; nothing it can run was not authorized then.
//! - `cue_arm` (Protocol 2.64) needs exactly what a `cue_go` of the same
//!   cue needs now: the clock later fires the armed GO, and every wait and
//!   follow it reaches, on the authority checked when it was armed.
//! - An edit of a list whose cue is waiting, or whose GO is armed, changes
//!   what the armed chains will run, so it also needs every class the
//!   edited chains run that the chains armed before the edit did not (an
//!   undo or redo of such an edit too).
//! - A cue's `run_script` step (Protocol 2.60) runs a script on the frame
//!   the step runs, which for a step after a wait or in a followed cue is a
//!   later frame whose show (and so the branches the script takes) is not
//!   known when the chain is armed. Such a step therefore needs the class of
//!   every action verb the script's source holds, in any branch
//!   ([`fm_script::Program::verbs`]): a bound on whatever it asks for when
//!   it runs. A source that does not parse needs nothing; its run fails.
//! - An edit of a script (`script_set`, and an undo or redo restoring one)
//!   that a waiting cue's armed chain runs needs every class the edited
//!   source's verbs need that the script's verbs did not, like an armed
//!   list edit. A script a cue runs cannot be removed.
//!
//! The other cue controls (standby, back, skip, stop, reset, disarm) never
//! perform an action and need only `transition`.

use fm_auth::CommandClass;
use fm_engine::Engine;
use fm_model::{CueAction, cue_chain_actions};
use fm_protocol::{CommandPayload, CueOperation};
use fm_script::Verb;

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
/// wait or delay, which performs nothing, and for a script step, which
/// needs the classes of its script's verbs ([`script_classes`]) instead.
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
        CueAction::Wait { .. } | CueAction::Delay { .. } | CueAction::RunScript { .. } => {
            return None;
        }
    })
}

/// The class the direct command equivalent to a script action of `verb`
/// needs (the class of its [`CueAction`], see [`cue_action_class`]).
#[must_use]
pub const fn verb_class(verb: Verb) -> CommandClass {
    match verb {
        Verb::Preview => CommandClass::SelectPreview,
        Verb::TitleText => CommandClass::EditProject,
        Verb::Gain | Verb::Mute | Verb::Unmute => CommandClass::ControlAudio,
        Verb::Cut
        | Verb::Transition
        | Verb::Stinger
        | Verb::Overlay
        | Verb::OverlayOff
        | Verb::TitleOn
        | Verb::TitleOff
        | Verb::Ftb
        | Verb::FtbOff
        | Verb::PlaylistStart
        | Verb::PlaylistStop
        | Verb::RecordStart
        | Verb::RecordStop
        | Verb::StreamStart
        | Verb::StreamStop
        | Verb::Play
        | Verb::Pause
        | Verb::Seek
        | Verb::Restart => CommandClass::Transition,
    }
}

fn verb_classes(verbs: impl IntoIterator<Item = Verb>) -> ClassSet {
    verbs.into_iter().map(verb_class).collect()
}

/// The classes a cue step running script `script` needs: those of every
/// verb its source holds, in any branch. Empty for an unknown script (the
/// step is refused) or a source that does not parse (its run fails).
pub fn script_classes(engine: &Engine, script: u64) -> ClassSet {
    verb_classes(engine.script_verbs(script).unwrap_or_default())
}

fn classes<'a>(engine: &Engine, actions: impl IntoIterator<Item = &'a CueAction>) -> ClassSet {
    let mut set = ClassSet::default();
    for action in actions {
        match action.script() {
            Some(script) => set.union(script_classes(engine, script)),
            None => set.extend(cue_action_class(action)),
        }
    }
    set
}

/// Every action the chains `list` has armed can still run, from the cues
/// `cues` (the list's own, or an edit of them): the waiting cue's from the
/// action it resumes at, then the armed GO's (Protocol 2.64).
fn armed_actions<'a>(list: &fm_model::CueList, cues: &'a [fm_model::Cue]) -> Vec<&'a CueAction> {
    let mut actions = Vec::new();
    if let Some(pending) = list.state.pending {
        actions.extend(cue_chain_actions(cues, pending.cue, pending.action));
    }
    if let Some(armed) = list.state.armed {
        actions.extend(cue_chain_actions(cues, armed.cue, 0));
    }
    actions
}

/// Whether a waiting cue's or an armed GO's chain runs script `script`.
fn armed_chain_runs(engine: &Engine, script: u64) -> bool {
    engine.show().cue_lists().iter().any(|list| {
        armed_actions(list, &list.cues)
            .into_iter()
            .any(|action| action.script() == Some(script))
    })
}

/// The classes of the show actions `payload` performs or arms on `engine`
/// now, beyond its own class (see the module documentation). Empty for a
/// command that runs no automation, and for one the engine will refuse
/// before anything runs (an unknown list, script, or cue).
pub fn action_classes(engine: &Engine, payload: &CommandPayload) -> ClassSet {
    match payload {
        CommandPayload::RunScript { script } => {
            let mut set = classes(engine, &engine.script_run_actions(*script));
            if engine.script_waits(*script) == Some(true) {
                set.union(script_classes(engine, *script));
            }
            set
        }
        CommandPayload::CueControl {
            list,
            operation: CueOperation::Go { cue } | CueOperation::Arm { cue, .. },
        } => engine
            .show()
            .cue_list(*list)
            .and_then(|found| cue.or(found.state.standby).map(|cue| (found, cue)))
            .map_or_else(ClassSet::default, |(found, cue)| {
                classes(engine, cue_chain_actions(&found.cues, cue, 0))
            }),
        CommandPayload::SetCueList { list, spec } => {
            let Some(found) = engine.show().cue_list(*list) else {
                return ClassSet::default();
            };
            if found.state.pending.is_none() && found.state.armed.is_none() {
                return ClassSet::default();
            }
            let armed = classes(engine, armed_actions(found, &found.cues));
            let edited: Vec<fm_model::Cue> = spec.cues.iter().map(crate::cue::domain_cue).collect();
            classes(engine, armed_actions(found, &edited)).without(armed)
        }
        CommandPayload::SetScript { script, spec } if armed_chain_runs(engine, *script) => {
            let edited = fm_script::parse(&spec.source)
                .map(|program| program.verbs())
                .unwrap_or_default();
            verb_classes(edited).without(script_classes(engine, *script))
        }
        _ => ClassSet::default(),
    }
}
