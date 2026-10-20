//! Edit ▸ Shortcuts… and the project's user shortcuts at runtime (RC-004).
//!
//! A project holds up to 64 named shortcuts. Each runs one thing: up to
//! sixteen cue actions together on one frame (no waits: a timed sequence
//! is a cue), an operation of a cue list (GO, Stop, Back, Skip, Reset), or
//! a project script. A shortcut answers a key on this console and an OSC
//! address, `/freemix/user/<name>`, on Studio's OSC listener. Both live in
//! the project, so a show carries its keys to every console.
//!
//! The window lists the shortcuts and edits one at a time as a draft. "Set
//! key" captures the next key pressed (with Ctrl or Cmd, Shift, and Alt);
//! a key the console already uses for one of its own commands, or another
//! shortcut's key or OSC name, is flagged and holds Save. Save sends the
//! whole shortcut (`shortcut_set`, a padlocked, undoable project edit);
//! Delete removes it (`shortcut_remove`).
//!
//! A key press fires an enabled shortcut (`shortcut_fire`, an operator
//! control) under the console keys' own gates: never while a dialog, a
//! menu, a keyboard-owning panel, or a text field has the keyboard, and
//! never again for the repeats of a held key. A fire this console cannot
//! send (no permission for one of its actions, disabled, a missing cue
//! list or script) is announced in a toast instead, as is any refusal from
//! the server.

use core::fmt::Write as _;

use egui::{Button, ComboBox, Event, Key, Modifiers, RichText, TextEdit, Ui};
use fm_protocol::{
    CueListSpec, CueListStatus, CueRunStatus, MAX_SHORTCUT_ACTIONS, MAX_SHORTCUT_NAME_BYTES,
    MAX_SHORTCUT_OSC_BYTES, MAX_SHORTCUTS, ShortcutSpec, ShortcutStatus, WireCue, WireCueAction,
    WireShortcutAction, WireShortcutCueOperation, WireShortcutKey,
};

use crate::gating::Hint;
use crate::l10n::{self, Arg, Locale, Msg};
use crate::theme::{ERROR, caption, muted_color, section_label};
use crate::toasts::ToastKind;
use crate::{StudioIntent, StudioShell, StudioUiState};

const DIALOG_WIDTH: f32 = 760.0;
const LIST_WIDTH: f32 = 200.0;
/// The space between the list and the editor, its divider in the middle.
const DIVIDER_GAP: f32 = 12.0;

/// A project shortcut action. Studio maps each to one protocol command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShortcutIntent {
    /// Creates or replaces one shortcut (`shortcut_set`): an undoable
    /// project edit.
    Set {
        shortcut: u64,
        spec: Box<ShortcutSpec>,
    },
    /// Deletes one shortcut (`shortcut_remove`): an undoable project edit.
    Remove { shortcut: u64 },
    /// Fires one shortcut (`shortcut_fire`): an operator control.
    Fire { shortcut: u64 },
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// A key the console handles itself, which no shortcut may take.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReservedKey {
    pub(crate) key: Key,
    pub(crate) modifiers: Modifiers,
    /// egui's logical match (extra Shift and Alt still reach the handler),
    /// else exact.
    pub(crate) logical: bool,
    /// The key text the keyboard reference shows for it.
    pub(crate) keys: &'static str,
}

const fn reserved(
    key: Key,
    modifiers: Modifiers,
    logical: bool,
    keys: &'static str,
) -> ReservedKey {
    ReservedKey {
        key,
        modifiers,
        logical,
        keys,
    }
}

const NONE: Modifiers = Modifiers::NONE;
const CMD: Modifiers = Modifiers::COMMAND;
const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);

/// Every key the console reads in every context (the help, preference,
/// and window keys) and while no panel owns the keyboard (the console
/// keys), as [`crate::keymap`]'s binding table lists them (a unit test
/// there keeps the two in step), then the keys that move and press the
/// keyboard focus.
pub(crate) const RESERVED_KEYS: &[ReservedKey] = &[
    reserved(
        Key::Comma,
        CMD,
        false,
        crate::preferences::PREFERENCES_SHORTCUT,
    ),
    reserved(
        Key::Equals,
        CMD,
        false,
        crate::preferences::UI_SCALE_SHORTCUT,
    ),
    reserved(Key::Plus, CMD, false, crate::preferences::UI_SCALE_SHORTCUT),
    reserved(
        Key::Minus,
        CMD,
        false,
        crate::preferences::UI_SCALE_SHORTCUT,
    ),
    reserved(
        Key::Num0,
        CMD,
        false,
        crate::preferences::UI_SCALE_RESET_SHORTCUT,
    ),
    reserved(Key::Questionmark, NONE, true, "? · F1"),
    reserved(Key::F1, NONE, false, "? · F1"),
    reserved(Key::G, CMD, true, crate::studio_color::COLOR_SHORTCUT),
    reserved(
        Key::A,
        CMD_SHIFT,
        true,
        crate::studio_animation::ANIMATION_SHORTCUT,
    ),
    reserved(
        Key::L,
        CMD_SHIFT,
        true,
        crate::scene_designer::SCENE_DESIGNER_SHORTCUT,
    ),
    reserved(Key::S, CMD_SHIFT, false, crate::keymap::SCRIPTS_SHORTCUT),
    reserved(Key::N, CMD_SHIFT, false, crate::keymap::LIVE_LAN_SHORTCUT),
    reserved(Key::B, NONE, false, "B"),
    reserved(Key::T, NONE, false, "T"),
    reserved(Key::PageUp, NONE, false, "PgUp / PgDn"),
    reserved(Key::PageDown, NONE, false, "PgUp / PgDn"),
    reserved(Key::End, NONE, false, "End"),
    reserved(Key::Escape, NONE, false, "Esc"),
    reserved(Key::C, NONE, false, "C"),
    reserved(Key::G, NONE, false, "G"),
    reserved(Key::Comma, NONE, false, ","),
    reserved(Key::Period, NONE, false, "."),
    reserved(Key::F, NONE, false, "F"),
    reserved(Key::F5, NONE, false, "F5 – F8"),
    reserved(Key::F6, NONE, false, "F5 – F8"),
    reserved(Key::F7, NONE, false, "F5 – F8"),
    reserved(Key::F8, NONE, false, "F5 – F8"),
    reserved(Key::Num1, NONE, false, "1 – 8"),
    reserved(Key::Num2, NONE, false, "1 – 8"),
    reserved(Key::Num3, NONE, false, "1 – 8"),
    reserved(Key::Num4, NONE, false, "1 – 8"),
    reserved(Key::Num5, NONE, false, "1 – 8"),
    reserved(Key::Num6, NONE, false, "1 – 8"),
    reserved(Key::Num7, NONE, false, "1 – 8"),
    reserved(Key::Num8, NONE, false, "1 – 8"),
    reserved(Key::Z, CMD, false, crate::UNDO_SHORTCUT),
    reserved(Key::Z, CMD_SHIFT, false, crate::REDO_SHORTCUT),
    reserved(Key::R, CMD_SHIFT, false, crate::keymap::RECORD_SHORTCUT),
    reserved(
        Key::O,
        CMD_SHIFT,
        false,
        crate::keymap::START_STREAMS_SHORTCUT,
    ),
    reserved(
        Key::K,
        CMD_SHIFT,
        false,
        crate::keymap::STOP_STREAMS_SHORTCUT,
    ),
    reserved(Key::M, CMD_SHIFT, false, crate::keymap::CUE_ARM_SHORTCUT),
    reserved(Key::D, CMD_SHIFT, false, crate::keymap::CUE_DISARM_SHORTCUT),
    reserved(Key::X, CMD_SHIFT, false, crate::keymap::CUE_STOP_SHORTCUT),
    reserved(Key::E, CMD_SHIFT, false, crate::keymap::CUE_RESET_SHORTCUT),
    // Keyboard focus: Tab moves it, Space and Enter press the focused key.
    reserved(Key::Tab, NONE, true, "Tab"),
    reserved(Key::Space, NONE, false, "Space"),
    reserved(Key::Enter, NONE, false, "Enter"),
];

/// How many of [`RESERVED_KEYS`] mirror the key map (the rest are focus
/// keys).
#[cfg(test)]
pub(crate) const MIRRORED_KEYS: usize = RESERVED_KEYS.len() - 3;

/// The press a shortcut key stands for, as egui reports it on Linux and
/// Windows (Ctrl is the command key).
const fn press(key: &WireShortcutKey) -> Modifiers {
    Modifiers {
        alt: key.alt,
        ctrl: key.command,
        shift: key.shift,
        mac_cmd: false,
        command: key.command,
    }
}

/// The modifiers a shortcut key requires, matched exactly.
const fn pattern(key: &WireShortcutKey) -> Modifiers {
    Modifiers {
        alt: key.alt,
        ctrl: false,
        shift: key.shift,
        mac_cmd: false,
        command: key.command,
    }
}

/// The console key a shortcut key would shadow, by its reference text.
#[must_use]
pub(crate) fn reserved_by(key: &WireShortcutKey) -> Option<&'static str> {
    let named = Key::from_name(&key.key)?;
    let pressed = press(key);
    RESERVED_KEYS
        .iter()
        .find(|reserved| {
            reserved.key == named
                && if reserved.logical {
                    pressed.matches_logically(reserved.modifiers)
                } else {
                    pressed.matches_exact(reserved.modifiers)
                }
        })
        .map(|reserved| reserved.keys)
}

/// `Ctrl+Shift+Alt+F9`: a shortcut key as the keyboard reference writes a
/// combination (see [`Locale::keys`] for the console's words).
#[must_use]
pub fn key_text(key: &WireShortcutKey) -> String {
    let mut text = String::new();
    for (held, name) in [
        (key.command, "Ctrl"),
        (key.shift, "Shift"),
        (key.alt, "Alt"),
    ] {
        if held {
            text.push_str(name);
            text.push('+');
        }
    }
    text.push_str(&key.key);
    text
}

/// What a key press during "Set key" means.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Captured {
    /// The key to bind.
    Key(WireShortcutKey),
    /// Esc alone: keep the key the draft had.
    Cancel,
    /// A key a shortcut cannot name (macOS's Control, a clipboard key, or
    /// a name too long).
    Unsupported,
}

/// Whether `key` is a modifier on its own, which a capture waits past.
const fn modifier_only(key: Key) -> bool {
    matches!(
        key,
        Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
    )
}

/// Takes every key press and typed text from `events` (nothing else reads
/// them while a capture is on) and returns what the first new press
/// means, if any came.
pub(crate) fn capture(events: &mut Vec<Event>) -> Option<Captured> {
    let mut captured = None;
    events.retain(|event| match event {
        Event::Key {
            key,
            pressed,
            repeat,
            modifiers,
            ..
        } => {
            if *pressed && !*repeat && !modifier_only(*key) && captured.is_none() {
                captured = Some(captured_key(*key, *modifiers));
            }
            false
        }
        Event::Text(_) | Event::Copy | Event::Cut | Event::Paste(_) => false,
        _ => true,
    });
    captured
}

fn captured_key(key: Key, modifiers: Modifiers) -> Captured {
    if key == Key::Escape && !modifiers.any() {
        return Captured::Cancel;
    }
    // macOS's Control key (Ctrl without Cmd) has no place in a shortcut,
    // which names Ctrl on Linux and Windows and Cmd on macOS.
    if (modifiers.ctrl && !modifiers.command) || matches!(key, Key::Copy | Key::Cut | Key::Paste) {
        return Captured::Unsupported;
    }
    let named = WireShortcutKey {
        key: key.name().to_owned(),
        command: modifiers.command,
        shift: modifiers.shift,
        alt: modifiers.alt,
    };
    if fm_protocol::shortcut_key_is_valid(&named) {
        Captured::Key(named)
    } else {
        Captured::Unsupported
    }
}

/// The shortcut a press of `key` with `modifiers` names: one whose key
/// matches exactly and does not shadow a console key.
#[must_use]
pub(crate) fn pressed(
    shortcuts: &[ShortcutStatus],
    key: Key,
    modifiers: Modifiers,
) -> Option<&ShortcutStatus> {
    shortcuts.iter().find(|shortcut| {
        shortcut.spec.key.as_ref().is_some_and(|bound| {
            Key::from_name(&bound.key) == Some(key)
                && modifiers.matches_exact(pattern(bound))
                && reserved_by(bound).is_none()
        })
    })
}

/// Removes this frame's presses of shortcut keys from `events`, so nothing
/// else reads them, and returns the shortcuts the new presses name, in
/// order. The repeats of a held key are swallowed without firing again.
pub(crate) fn take_presses(events: &mut Vec<Event>, shortcuts: &[ShortcutStatus]) -> Vec<u64> {
    let mut fired = Vec::new();
    events.retain(|event| {
        let Event::Key {
            key,
            pressed: true,
            repeat,
            modifiers,
            ..
        } = event
        else {
            return true;
        };
        let Some(shortcut) = pressed(shortcuts, *key, *modifiers) else {
            return true;
        };
        if !repeat {
            fired.push(shortcut.id);
        }
        false
    });
    fired
}

// ---------------------------------------------------------------------------
// Gates
// ---------------------------------------------------------------------------

/// The replicated shortcuts, empty before a snapshot.
pub(crate) fn replicated(state: &StudioUiState) -> &[ShortcutStatus] {
    state
        .project_state
        .as_ref()
        .map_or(&[][..], fm_ui_model::ProjectState::shortcuts)
}

/// The cue list a cue action list stands in for, so the cue GO's
/// permission check applies to it.
fn as_list(actions: Vec<WireCueAction>) -> CueListStatus {
    CueListStatus {
        id: 0,
        spec: CueListSpec {
            name: String::new(),
            cues: vec![WireCue {
                id: 1,
                name: String::new(),
                auto_follow: false,
                actions,
            }],
        },
        state: CueRunStatus::default(),
    }
}

/// Why this session may not fire `shortcut`, if it may not: the session's
/// transition gate, a disabled shortcut, a cue list or script the project
/// no longer has, then a permission one of its actions needs (for a cue
/// GO, every cue the GO runs; for a script, every verb it holds).
#[must_use]
pub fn fire_block(state: &StudioUiState, shortcut: &ShortcutStatus) -> Option<&'static str> {
    crate::cues::control_block(state).or_else(|| {
        if !shortcut.spec.enabled {
            return Some(Msg::UserShortcutReasonDisabled.en());
        }
        let project = state.project_state.as_ref()?;
        let scripts = project.scripts();
        match &shortcut.spec.action {
            WireShortcutAction::Actions(actions) => {
                crate::cues::chain_block(state, scripts, &as_list(actions.clone()), 1)
            }
            WireShortcutAction::Cue { list, operation } => {
                let Some(found) = project.cue_lists().iter().find(|found| found.id == *list) else {
                    return Some(Msg::UserShortcutReasonNoList.en());
                };
                match operation {
                    WireShortcutCueOperation::Go { cue } => {
                        let cue = cue.or(found.state.standby)?;
                        crate::cues::chain_block(state, scripts, found, cue)
                    }
                    _ => None,
                }
            }
            WireShortcutAction::Script { script } => {
                if scripts.iter().any(|found| found.id == *script) {
                    let run = vec![WireCueAction::RunScript { script: *script }];
                    crate::cues::chain_block(state, scripts, &as_list(run), 1)
                } else {
                    Some(Msg::UserShortcutReasonNoScript.en())
                }
            }
        }
    })
}

/// The shortcut OSC name `name` answers to.
#[must_use]
pub fn by_osc<'a>(shortcuts: &'a [ShortcutStatus], name: &str) -> Option<&'a ShortcutStatus> {
    shortcuts
        .iter()
        .find(|shortcut| shortcut.spec.osc.as_deref() == Some(name))
}

/// Which other shortcut already uses `key`, if one does.
fn key_taken<'a>(
    shortcuts: &'a [ShortcutStatus],
    shortcut: u64,
    key: &WireShortcutKey,
) -> Option<&'a ShortcutStatus> {
    shortcuts
        .iter()
        .find(|other| other.id != shortcut && other.spec.key.as_ref() == Some(key))
}

/// Which other shortcut already answers OSC name `name`, if one does.
fn osc_taken<'a>(
    shortcuts: &'a [ShortcutStatus],
    shortcut: u64,
    name: &str,
) -> Option<&'a ShortcutStatus> {
    shortcuts
        .iter()
        .find(|other| other.id != shortcut && other.spec.osc.as_deref() == Some(name))
}

/// Why the draft's action cannot be saved, if it cannot.
fn action_block(action: &WireShortcutAction) -> Option<&'static str> {
    match action {
        WireShortcutAction::Actions(actions) if actions.is_empty() => {
            Some(Msg::UserShortcutReasonNoActions.en())
        }
        WireShortcutAction::Actions(actions) if actions.len() > MAX_SHORTCUT_ACTIONS => {
            Some(Msg::UserShortcutReasonTooManyActions.en())
        }
        WireShortcutAction::Actions(actions)
            if actions.iter().any(|action| {
                matches!(
                    action,
                    WireCueAction::Wait { .. } | WireCueAction::Delay { .. }
                )
            }) =>
        {
            Some(Msg::UserShortcutReasonWaits.en())
        }
        WireShortcutAction::Cue { list: 0, .. } => Some(Msg::UserShortcutReasonNoList.en()),
        WireShortcutAction::Script { script: 0 } => Some(Msg::UserShortcutReasonNoScript.en()),
        _ => None,
    }
}

/// Why the draft cannot be saved, if it cannot: the edit gate, then an
/// unchanged draft, its name, key, OSC name, and action, and the project's
/// limit.
#[must_use]
pub fn save_block(
    state: &StudioUiState,
    shortcuts: &[ShortcutStatus],
    shortcut: u64,
    spec: &ShortcutSpec,
    dirty: bool,
) -> Option<&'static str> {
    crate::cues::edit_block(state).or_else(|| {
        if !dirty {
            Some(Msg::UserShortcutReasonUnchanged.en())
        } else if !fm_protocol::shortcut_name_is_valid(&spec.name) {
            Some(Msg::ScriptReasonName.en())
        } else if spec
            .key
            .as_ref()
            .is_some_and(|key| reserved_by(key).is_some())
        {
            Some(Msg::UserShortcutReasonReserved.en())
        } else if spec
            .key
            .as_ref()
            .is_some_and(|key| key_taken(shortcuts, shortcut, key).is_some())
        {
            Some(Msg::UserShortcutReasonKeyTaken.en())
        } else if spec
            .osc
            .as_deref()
            .is_some_and(|name| !fm_protocol::shortcut_osc_is_valid(name))
        {
            Some(Msg::UserShortcutReasonOsc.en())
        } else if spec
            .osc
            .as_deref()
            .is_some_and(|name| osc_taken(shortcuts, shortcut, name).is_some())
        {
            Some(Msg::UserShortcutReasonOscTaken.en())
        } else if let Some(reason) = action_block(&spec.action) {
            Some(reason)
        } else if fm_protocol::validate_shortcut_spec(spec).is_err() {
            Some(Msg::UserShortcutReasonInvalid.en())
        } else if shortcuts.len() >= MAX_SHORTCUTS && !shortcuts.iter().any(|s| s.id == shortcut) {
            Some(Msg::UserShortcutReasonLimit.en())
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// The window's open state and the draft being edited.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ShortcutsWindow {
    pub(crate) open: bool,
    /// The shortcut the draft edits; a new shortcut's id until it is saved.
    pub(crate) selected: Option<u64>,
    pub(crate) draft: Option<ShortcutSpec>,
    /// The replicated definition the draft was loaded from (`None` for a
    /// new shortcut), so an unchanged draft follows the project.
    loaded: Option<ShortcutSpec>,
    /// Whether the next key press binds the draft's key.
    pub(crate) capturing: bool,
    /// The last capture pressed a key a shortcut cannot name.
    unsupported: bool,
}

impl ShortcutsWindow {
    /// Edits `shortcut` (or nothing when the project has none).
    pub(crate) fn select(&mut self, shortcuts: &[ShortcutStatus], shortcut: Option<u64>) {
        let found = shortcut.and_then(|id| shortcuts.iter().find(|found| found.id == id));
        self.selected = found.map(|found| found.id);
        self.draft = found.map(|found| found.spec.clone());
        self.loaded.clone_from(&self.draft);
        self.capturing = false;
        self.unsupported = false;
    }

    /// Starts a new draft under the next free id: cue actions, none yet.
    pub(crate) fn new_draft(&mut self, shortcuts: &[ShortcutStatus]) {
        let next = shortcuts.iter().map(|found| found.id).max().unwrap_or(0) + 1;
        self.selected = Some(next);
        self.draft = Some(ShortcutSpec {
            name: format!("Shortcut {next}"),
            enabled: true,
            key: None,
            osc: None,
            action: WireShortcutAction::Actions(Vec::new()),
        });
        self.loaded = None;
        self.capturing = false;
        self.unsupported = false;
    }

    /// Whether the draft differs from what it was loaded from.
    pub(crate) fn dirty(&self) -> bool {
        self.draft != self.loaded
    }

    /// Keeps the draft in step with the replicated shortcuts: an unchanged
    /// draft follows its shortcut, a removed one moves to the first, and a
    /// just-saved draft becomes the loaded one.
    pub(crate) fn follow(&mut self, shortcuts: &[ShortcutStatus]) {
        let Some(id) = self.selected else {
            if let Some(first) = shortcuts.first() {
                self.select(shortcuts, Some(first.id));
            }
            return;
        };
        let replicated = shortcuts
            .iter()
            .find(|found| found.id == id)
            .map(|found| found.spec.clone());
        if self.loaded == replicated {
            return;
        }
        match (&self.loaded, replicated) {
            (Some(_), None) => {
                let first = shortcuts.first().map(|found| found.id);
                self.select(shortcuts, first);
            }
            (_, Some(replicated)) if Some(&replicated) == self.draft.as_ref() => {
                self.loaded = Some(replicated);
            }
            (Some(_), Some(_)) if !self.dirty() => {
                let capturing = self.capturing;
                self.select(shortcuts, Some(id));
                self.capturing = capturing;
            }
            _ => {}
        }
    }
}

impl StudioShell {
    /// Opens the Shortcuts window on the selected or first shortcut.
    pub(crate) fn open_user_shortcuts(&mut self, state: &StudioUiState) {
        let shortcuts = replicated(state);
        if !self.user_shortcuts.open {
            let selected = self
                .user_shortcuts
                .selected
                .filter(|id| shortcuts.iter().any(|found| found.id == *id))
                .or_else(|| shortcuts.first().map(|found| found.id));
            self.user_shortcuts.select(shortcuts, selected);
        }
        self.user_shortcuts.open = true;
    }

    /// Draws the Shortcuts window when open.
    pub(crate) fn draw_user_shortcuts_window(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        if !self.user_shortcuts.open {
            return;
        }
        let l = self.locale;
        let window = &mut self.user_shortcuts;
        window.follow(replicated(state));
        if window.capturing {
            match context.input_mut(|input| capture(&mut input.events)) {
                Some(Captured::Key(key)) => {
                    if let Some(draft) = &mut window.draft {
                        draft.key = Some(key);
                    }
                    window.capturing = false;
                    window.unsupported = false;
                }
                Some(Captured::Cancel) => window.capturing = false,
                Some(Captured::Unsupported) => {
                    window.capturing = false;
                    window.unsupported = true;
                }
                None => {}
            }
        }
        let close = crate::menu::help_modal(
            context,
            "studio-user-shortcuts",
            l.text(Msg::UserShortcutsTitle),
            DIALOG_WIDTH,
            |ui| draw_contents(ui, state, window, intents),
        );
        if close && !self.user_shortcuts.capturing {
            self.user_shortcuts.open = false;
        }
    }

    /// Fires the shortcuts this frame's key presses name, under the console
    /// keys' gates (no dialog, menu, keyboard-owning panel, or text field
    /// has the keyboard); a fire this session cannot send is announced.
    pub(crate) fn fire_user_shortcut_keys(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        if self.console_keys_blocked(ui.ctx()) || ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let shortcuts = replicated(state);
        if shortcuts.is_empty() {
            return;
        }
        let fired = ui.input_mut(|input| take_presses(&mut input.events, shortcuts));
        for shortcut in fired {
            if let Some(intent) = self.user_shortcut_intent(state, shortcut) {
                intents.push(intent);
            }
        }
    }

    /// The intent that fires `shortcut`, or `None` after announcing why
    /// this session cannot.
    fn user_shortcut_intent(
        &mut self,
        state: &StudioUiState,
        shortcut: u64,
    ) -> Option<StudioIntent> {
        let found = replicated(state)
            .iter()
            .find(|found| found.id == shortcut)?;
        match fire_block(state, found) {
            None => Some(StudioIntent::Shortcut(ShortcutIntent::Fire { shortcut })),
            Some(reason) => {
                let l = self.locale;
                let message = l.format(
                    Msg::UserShortcutNotFired,
                    &[
                        ("name", Arg::Str(&found.spec.name)),
                        ("reason", Arg::Str(l.tr(reason))),
                    ],
                );
                self.toasts.push(ToastKind::Error, message);
                None
            }
        }
    }

    /// The intent an OSC `/freemix/user/<name>` message sends: the fire of
    /// the shortcut with that OSC name, under the same gates as its key.
    /// `None` (after announcing why, for a known name) when it cannot fire.
    pub fn user_shortcut_osc_intent(
        &mut self,
        state: &StudioUiState,
        name: &str,
    ) -> Option<StudioIntent> {
        let shortcut = by_osc(replicated(state), name)?.id;
        self.user_shortcut_intent(state, shortcut)
    }
}

fn draw_contents(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut ShortcutsWindow,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    ui.spacing_mut().interact_size.y = 24.0;
    ui.label(caption(l.text(Msg::UserShortcutsIntro)));
    ui.add_space(6.0);
    let shortcuts = replicated(state);
    let row = ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(LIST_WIDTH);
            draw_list(ui, state, window, shortcuts);
        });
        let (gap, _) = ui.allocate_exact_size(egui::vec2(DIVIDER_GAP, 0.0), egui::Sense::hover());
        ui.vertical(|ui| {
            if window.draft.is_some() {
                draw_editor(ui, state, window, shortcuts, intents);
            } else {
                ui.label(RichText::new(l.text(Msg::UserShortcutsEmpty)).color(muted_color()));
            }
        });
        gap.center().x
    });
    ui.painter().vline(
        row.inner,
        row.response.rect.y_range(),
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
}

/// A list row's text: the name, then its key in the console's words.
fn row_text(l: Locale, shortcut: &ShortcutStatus) -> String {
    let mut text = shortcut.spec.name.clone();
    if let Some(key) = &shortcut.spec.key {
        text.push_str("  ");
        text.push_str(&l.keys(&key_text(key)));
    }
    if !shortcut.spec.enabled {
        text = l.format(Msg::UserShortcutListOff, &[("name", Arg::Str(&text))]);
    }
    text
}

fn draw_list(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut ShortcutsWindow,
    shortcuts: &[ShortcutStatus],
) {
    let l = l10n::locale(ui.ctx());
    ui.label(section_label(l.text(Msg::UserShortcutsTitle)));
    if shortcuts.is_empty() {
        ui.label(RichText::new(l.text(Msg::UserShortcutsEmpty)).color(muted_color()));
    }
    for shortcut in shortcuts {
        let selected = window.selected == Some(shortcut.id);
        if ui
            .selectable_label(selected, row_text(l, shortcut))
            .on_hover_text(l.format(
                Msg::UserShortcutListHover,
                &[("name", Arg::Str(&shortcut.spec.name))],
            ))
            .clicked()
            && !selected
        {
            window.select(shortcuts, Some(shortcut.id));
        }
    }
    ui.add_space(4.0);
    let block = crate::cues::edit_block(state).or_else(|| {
        (shortcuts.len() >= MAX_SHORTCUTS).then_some(Msg::UserShortcutReasonLimit.en())
    });
    if ui
        .add_enabled(block.is_none(), Button::new(l.text(Msg::UserShortcutNew)))
        .hint(l.text(Msg::UserShortcutNewHover), block.unwrap_or(""))
        .clicked()
    {
        window.new_draft(shortcuts);
    }
}

/// What a shortcut runs, as the action picker names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActionKind {
    Actions,
    Cue,
    Script,
}

impl ActionKind {
    const ALL: [Self; 3] = [Self::Actions, Self::Cue, Self::Script];

    const fn of(action: &WireShortcutAction) -> Self {
        match action {
            WireShortcutAction::Actions(_) => Self::Actions,
            WireShortcutAction::Cue { .. } => Self::Cue,
            WireShortcutAction::Script { .. } => Self::Script,
        }
    }

    const fn label(self) -> Msg {
        match self {
            Self::Actions => Msg::UserShortcutKindActions,
            Self::Cue => Msg::UserShortcutKindCue,
            Self::Script => Msg::UserShortcutKindScript,
        }
    }

    /// A fresh action of this kind: no cue actions yet, a GO of the first
    /// list's standby cue, or the first script.
    fn fresh(self, state: &StudioUiState) -> WireShortcutAction {
        let project = state.project_state.as_ref();
        match self {
            Self::Actions => WireShortcutAction::Actions(Vec::new()),
            Self::Cue => WireShortcutAction::Cue {
                list: project
                    .and_then(|project| project.cue_lists().first())
                    .map_or(0, |list| list.id),
                operation: WireShortcutCueOperation::Go { cue: None },
            },
            Self::Script => WireShortcutAction::Script {
                script: project
                    .and_then(|project| project.scripts().first())
                    .map_or(0, |script| script.id),
            },
        }
    }
}

/// A cue list operation as the operation picker names it.
const fn operation_label(operation: WireShortcutCueOperation) -> Msg {
    match operation {
        WireShortcutCueOperation::Go { cue: None } => Msg::UserShortcutOpGo,
        WireShortcutCueOperation::Go { cue: Some(_) } => Msg::UserShortcutOpGoCue,
        WireShortcutCueOperation::Stop => Msg::UserShortcutOpStop,
        WireShortcutCueOperation::Back => Msg::UserShortcutOpBack,
        WireShortcutCueOperation::Skip => Msg::UserShortcutOpSkip,
        WireShortcutCueOperation::Reset => Msg::UserShortcutOpReset,
    }
}

#[allow(clippy::too_many_lines)] // one immediate-mode editor
fn draw_editor(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut ShortcutsWindow,
    shortcuts: &[ShortcutStatus],
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let Some(shortcut) = window.selected else {
        return;
    };
    let saved = shortcuts.iter().any(|found| found.id == shortcut);
    let edit = crate::cues::edit_block(state);
    let dirty = window.dirty();
    let capturing = window.capturing;
    let unsupported = window.unsupported;
    let Some(draft) = window.draft.as_mut() else {
        return;
    };

    ui.horizontal(|ui| {
        ui.label(l.text(Msg::UserShortcutNameLabel));
        ui.add_enabled(
            edit.is_none(),
            TextEdit::singleline(&mut draft.name)
                .char_limit(MAX_SHORTCUT_NAME_BYTES)
                .desired_width(260.0),
        )
        .hint(l.text(Msg::UserShortcutNameHover), edit.unwrap_or(""));
        ui.add_enabled(
            edit.is_none(),
            egui::Checkbox::new(&mut draft.enabled, l.text(Msg::UserShortcutEnabled)),
        )
        .hint(l.text(Msg::UserShortcutEnabledHover), edit.unwrap_or(""));
    });

    // The key: what it is, Set key (capture), and Clear key.
    let mut start_capture = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(l.text(Msg::UserShortcutKeyLabel));
        let shown = if capturing {
            RichText::new(l.text(Msg::UserShortcutKeyCapturing)).color(crate::theme::accent_color())
        } else {
            match &draft.key {
                Some(key) => crate::theme::mono(l.keys(&key_text(key))),
                None => RichText::new(l.text(Msg::UserShortcutKeyNone)).color(muted_color()),
            }
        };
        ui.label(shown)
            .on_hover_text(l.text(Msg::UserShortcutKeyHover));
        if ui
            .add_enabled(
                edit.is_none() && !capturing,
                Button::new(l.text(Msg::UserShortcutSetKey)),
            )
            .hint(
                l.text(Msg::UserShortcutSetKeyHover),
                edit.unwrap_or(Msg::UserShortcutReasonCapturing.en()),
            )
            .clicked()
        {
            start_capture = true;
        }
        let clear = edit.or_else(|| {
            draft
                .key
                .is_none()
                .then_some(Msg::UserShortcutReasonNoKey.en())
        });
        if ui
            .add_enabled(
                clear.is_none(),
                Button::new(l.text(Msg::UserShortcutClearKey)),
            )
            .hint(l.text(Msg::UserShortcutClearKeyHover), clear.unwrap_or(""))
            .clicked()
        {
            draft.key = None;
        }
    });
    if let Some(key) = &draft.key {
        if let Some(keys) = reserved_by(key) {
            ui.label(
                RichText::new(l.format(
                    Msg::UserShortcutKeyReserved,
                    &[("keys", Arg::Str(&l.keys(keys)))],
                ))
                .color(ERROR),
            );
        } else if let Some(other) = key_taken(shortcuts, shortcut, key) {
            ui.label(
                RichText::new(l.format(
                    Msg::UserShortcutKeyTaken,
                    &[("name", Arg::Str(&other.spec.name))],
                ))
                .color(ERROR),
            );
        }
    }
    if unsupported {
        ui.label(RichText::new(l.text(Msg::UserShortcutKeyUnsupported)).color(ERROR));
    }

    // The OSC name.
    ui.horizontal_wrapped(|ui| {
        ui.label(l.text(Msg::UserShortcutOscLabel));
        let mut text = draft.osc.clone().unwrap_or_default();
        let response = ui
            .add_enabled(
                edit.is_none(),
                TextEdit::singleline(&mut text)
                    .char_limit(MAX_SHORTCUT_OSC_BYTES)
                    .desired_width(180.0),
            )
            .hint(l.text(Msg::UserShortcutOscHover), edit.unwrap_or(""));
        if response.changed() {
            draft.osc = (!text.is_empty()).then_some(text);
        }
        if let Some(name) = &draft.osc {
            ui.label(crate::theme::mono(format!(
                "{}{name}",
                fm_protocol::SHORTCUT_OSC_PREFIX
            )))
            .on_hover_text(l.text(Msg::UserShortcutOscAddressHover));
        }
    });
    if let Some(name) = &draft.osc {
        if !fm_protocol::shortcut_osc_is_valid(name) {
            ui.label(RichText::new(l.text(Msg::UserShortcutReasonOsc)).color(ERROR));
        } else if let Some(other) = osc_taken(shortcuts, shortcut, name) {
            ui.label(
                RichText::new(l.format(
                    Msg::UserShortcutOscTaken,
                    &[("name", Arg::Str(&other.spec.name))],
                ))
                .color(ERROR),
            );
        }
    }

    ui.add_space(4.0);
    draw_action(ui, state, draft, edit);
    ui.add_space(4.0);

    let spec = draft.clone();
    ui.horizontal(|ui| {
        let save = save_block(state, shortcuts, shortcut, &spec, dirty);
        if ui
            .add_enabled(save.is_none(), Button::new(l.text(Msg::UserShortcutSave)))
            .hint(l.text(Msg::UserShortcutSaveHover), save.unwrap_or(""))
            .clicked()
        {
            intents.push(StudioIntent::Shortcut(ShortcutIntent::Set {
                shortcut,
                spec: Box::new(spec.clone()),
            }));
        }
        let revert = (!dirty).then_some(Msg::UserShortcutReasonUnchanged.en());
        if ui
            .add_enabled(
                revert.is_none(),
                Button::new(l.text(Msg::UserShortcutRevert)),
            )
            .hint(l.text(Msg::UserShortcutRevertHover), revert.unwrap_or(""))
            .clicked()
        {
            let keep = saved.then_some(shortcut);
            window.select(
                shortcuts,
                keep.or_else(|| shortcuts.first().map(|found| found.id)),
            );
        }
        let fire = if !saved {
            Some(Msg::UserShortcutReasonNotSaved.en())
        } else if dirty {
            Some(Msg::UserShortcutReasonUnsaved.en())
        } else {
            shortcuts
                .iter()
                .find(|found| found.id == shortcut)
                .and_then(|found| fire_block(state, found))
        };
        if ui
            .add_enabled(fire.is_none(), Button::new(l.text(Msg::UserShortcutFire)))
            .hint(l.text(Msg::UserShortcutFireHover), fire.unwrap_or(""))
            .clicked()
        {
            intents.push(StudioIntent::Shortcut(ShortcutIntent::Fire { shortcut }));
        }
        let remove = edit.or_else(|| (!saved).then_some(Msg::UserShortcutReasonNotSaved.en()));
        if ui
            .add_enabled(
                remove.is_none(),
                Button::new(l.text(Msg::UserShortcutRemove)),
            )
            .hint(l.text(Msg::UserShortcutRemoveHover), remove.unwrap_or(""))
            .clicked()
        {
            intents.push(StudioIntent::Shortcut(ShortcutIntent::Remove { shortcut }));
        }
    });
    if let Some(report) = state
        .project_state
        .as_ref()
        .and_then(fm_ui_model::ProjectState::last_shortcut_fire)
        .filter(|report| report.shortcut == shortcut)
    {
        ui.label(
            RichText::new(l.format(
                Msg::UserShortcutLastFire,
                &[
                    ("count", Arg::Count(u64::from(report.actions))),
                    ("frame", Arg::Plain(report.frame)),
                ],
            ))
            .color(muted_color()),
        )
        .on_hover_text(l.text(Msg::UserShortcutLastFireHover));
    }
    if start_capture {
        window.capturing = true;
        window.unsupported = false;
    }
}

/// The draft's action: its kind, then the cue actions, the cue list and
/// operation, or the script.
#[allow(clippy::too_many_lines)] // one immediate-mode picker group
fn draw_action(
    ui: &mut Ui,
    state: &StudioUiState,
    draft: &mut ShortcutSpec,
    edit: Option<&'static str>,
) {
    let l = l10n::locale(ui.ctx());
    let current = ActionKind::of(&draft.action);
    ui.horizontal(|ui| {
        ui.label(l.text(Msg::UserShortcutActionLabel));
        ui.add_enabled_ui(edit.is_none(), |ui| {
            ComboBox::from_id_salt("studio-user-shortcut-kind")
                .selected_text(l.text(current.label()))
                .show_ui(ui, |ui| {
                    for kind in ActionKind::ALL {
                        if ui
                            .add(Button::selectable(kind == current, l.text(kind.label())))
                            .on_hover_text(l.text(Msg::UserShortcutKindHover))
                            .clicked()
                            && kind != current
                        {
                            draft.action = kind.fresh(state);
                        }
                    }
                })
                .response
                .hint(l.text(Msg::UserShortcutKindHover), edit.unwrap_or(""));
        });
    });
    let (Some(view), Some(project)) = (state.view.as_ref(), state.project_state.as_ref()) else {
        return;
    };
    match &mut draft.action {
        WireShortcutAction::Actions(actions) => {
            ui.label(caption(l.text(Msg::UserShortcutActionsNote)));
            crate::cues::draw_action_list(ui, view, project, actions, edit);
        }
        WireShortcutAction::Cue { list, operation } => {
            let lists = project.cue_lists();
            ui.horizontal_wrapped(|ui| {
                let name = lists.iter().find(|found| found.id == *list).map_or_else(
                    || l.text(Msg::UserShortcutNoneChosen).to_owned(),
                    |found| found.spec.name.clone(),
                );
                let reason = edit.or_else(|| {
                    lists
                        .is_empty()
                        .then_some(Msg::UserShortcutReasonNoLists.en())
                });
                ui.add_enabled_ui(reason.is_none(), |ui| {
                    ComboBox::from_id_salt("studio-user-shortcut-list")
                        .selected_text(name)
                        .show_ui(ui, |ui| {
                            for found in lists {
                                if ui
                                    .add(Button::selectable(found.id == *list, &found.spec.name))
                                    .on_hover_text(l.text(Msg::UserShortcutListPickHover))
                                    .clicked()
                                    && found.id != *list
                                {
                                    *list = found.id;
                                    if let WireShortcutCueOperation::Go { cue } = operation {
                                        *cue = None;
                                    }
                                }
                            }
                        })
                        .response
                        .hint(l.text(Msg::UserShortcutListPickHover), reason.unwrap_or(""));
                });
                let cues = lists
                    .iter()
                    .find(|found| found.id == *list)
                    .map_or(&[][..], |found| found.spec.cues.as_slice());
                let first_cue = cues.first().map(|cue| cue.id);
                let choices = [
                    WireShortcutCueOperation::Go { cue: None },
                    WireShortcutCueOperation::Go {
                        cue: Some(first_cue.unwrap_or(1)),
                    },
                    WireShortcutCueOperation::Stop,
                    WireShortcutCueOperation::Back,
                    WireShortcutCueOperation::Skip,
                    WireShortcutCueOperation::Reset,
                ];
                ui.add_enabled_ui(edit.is_none(), |ui| {
                    ComboBox::from_id_salt("studio-user-shortcut-operation")
                        .selected_text(l.text(operation_label(*operation)))
                        .show_ui(ui, |ui| {
                            for choice in choices {
                                let same = operation_label(choice) == operation_label(*operation);
                                let usable = !matches!(
                                    choice,
                                    WireShortcutCueOperation::Go { cue: Some(_) }
                                ) || first_cue.is_some();
                                if ui
                                    .add_enabled(
                                        usable,
                                        Button::selectable(same, l.text(operation_label(choice))),
                                    )
                                    .hint(
                                        l.text(Msg::UserShortcutOperationHover),
                                        Msg::UserShortcutReasonNoCues.en(),
                                    )
                                    .clicked()
                                    && !same
                                {
                                    *operation = choice;
                                }
                            }
                        })
                        .response
                        .hint(l.text(Msg::UserShortcutOperationHover), edit.unwrap_or(""));
                });
                if let WireShortcutCueOperation::Go { cue: Some(cue) } = operation {
                    let shown = cues
                        .iter()
                        .find(|found| found.id == *cue)
                        .map_or_else(|| cue.to_string(), |found| found.name.clone());
                    ui.add_enabled_ui(edit.is_none(), |ui| {
                        ComboBox::from_id_salt("studio-user-shortcut-cue")
                            .selected_text(shown)
                            .show_ui(ui, |ui| {
                                for found in cues {
                                    if ui
                                        .add(Button::selectable(found.id == *cue, &found.name))
                                        .on_hover_text(l.text(Msg::UserShortcutCuePickHover))
                                        .clicked()
                                    {
                                        *cue = found.id;
                                    }
                                }
                            })
                            .response
                            .hint(l.text(Msg::UserShortcutCuePickHover), edit.unwrap_or(""));
                    });
                }
            });
        }
        WireShortcutAction::Script { script } => {
            let scripts = project.scripts();
            ui.horizontal(|ui| {
                let name = scripts
                    .iter()
                    .find(|found| found.id == *script)
                    .map_or_else(
                        || l.text(Msg::UserShortcutNoneChosen).to_owned(),
                        |found| found.spec.name.clone(),
                    );
                let reason = edit.or_else(|| {
                    scripts
                        .is_empty()
                        .then_some(Msg::UserShortcutReasonNoScripts.en())
                });
                ui.add_enabled_ui(reason.is_none(), |ui| {
                    ComboBox::from_id_salt("studio-user-shortcut-script")
                        .selected_text(name)
                        .show_ui(ui, |ui| {
                            for found in scripts {
                                if ui
                                    .add(Button::selectable(found.id == *script, &found.spec.name))
                                    .on_hover_text(l.text(Msg::UserShortcutScriptPickHover))
                                    .clicked()
                                {
                                    *script = found.id;
                                }
                            }
                        })
                        .response
                        .hint(
                            l.text(Msg::UserShortcutScriptPickHover),
                            reason.unwrap_or(""),
                        );
                });
            });
        }
    }
}

/// The keyboard reference's Project shortcuts group: each shortcut with a
/// key or OSC name, then where to edit them.
pub(crate) fn reference_rows(l: Locale, state: &StudioUiState) -> Vec<(String, String)> {
    replicated(state)
        .iter()
        .filter(|shortcut| shortcut.spec.key.is_some() || shortcut.spec.osc.is_some())
        .map(|shortcut| {
            let keys = shortcut.spec.key.as_ref().map_or_else(
                || l.text(Msg::UserShortcutOscOnly).to_owned(),
                |key| l.keys(&key_text(key)),
            );
            let mut action = shortcut.spec.name.clone();
            if let Some(name) = &shortcut.spec.osc {
                let _ = write!(action, "  ({}{name})", fm_protocol::SHORTCUT_OSC_PREFIX);
            }
            if !shortcut.spec.enabled {
                action = l.format(Msg::UserShortcutListOff, &[("name", Arg::Str(&action))]);
            }
            (keys, action)
        })
        .collect()
}

#[cfg(test)]
mod tests;
