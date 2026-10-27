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
//! key" captures the next key or mouse button pressed (with Ctrl or Cmd,
//! Shift, and Alt; the middle, back, and forward buttons, which may also
//! be either stroke of a chord);
//! a key the console already uses for one of its own commands, or another
//! shortcut's key or OSC name, is flagged and holds Save. Save sends the
//! whole shortcut (`shortcut_set`, a padlocked, undoable project edit);
//! Delete removes it (`shortcut_remove`).
//!
//! A key "answers in" the whole console, one panel (while it has the
//! focus), or one panel's pop-out window (while that window has the
//! system's focus; each popped-out panel's window reads its own presses).
//!
//! A key press fires an enabled shortcut (`shortcut_fire`, an operator
//! control) under the console keys' own gates: never while a dialog, a
//! menu, a keyboard-owning panel, or a text field has the keyboard, and
//! never again for the repeats of a held key. A fire this console cannot
//! send (no permission for one of its actions, disabled, a missing cue
//! list or script) is announced in a toast instead, as is any refusal from
//! the server.

use core::fmt::Write as _;

use egui::{Button, ComboBox, Event, Key, Modifiers, PointerButton, RichText, TextEdit, Ui};
use fm_preset_file::shortcuts::{ShortcutImportError, ShortcutImportPlan, plan_shortcut_import};
use fm_protocol::{
    CueListSpec, CueListStatus, CueRunStatus, MAX_SHORTCUT_ACTIONS, MAX_SHORTCUT_NAME_BYTES,
    MAX_SHORTCUT_OSC_BYTES, MAX_SHORTCUTS, ShortcutSpec, ShortcutStatus, WireCue, WireCueAction,
    WireShortcutAction, WireShortcutCueOperation, WireShortcutKey,
};
use fm_types::ProjectId;

use crate::{ConsoleKeymap, ConsoleStroke};

use crate::gating::Hint;
use crate::l10n::{self, Arg, Locale, Msg};
use crate::preset_files::PresetFileRequest;
use crate::shortcut_keys::{MAX_KEY_OVERRIDES, ShortcutKeyOverrides};
use crate::theme::{
    ACCENT_FILL, ERROR, accent_color, caption, empty_state, field_label, muted_color,
    section_label, small_button, text_color,
};
use crate::toasts::ToastKind;
use crate::{StudioIntent, StudioShell, StudioUiState};

const DIALOG_WIDTH: f32 = 760.0;
const LIST_WIDTH: f32 = 200.0;
/// The space between the list and the editor, its divider in the middle.
const DIVIDER_GAP: f32 = 12.0;
/// Width of the editor's field label column, so the fields line up.
const FORM_LABEL_WIDTH: f32 = 118.0;
/// What a label cell adds past its width before the field beside it.
const FORM_LABEL_GAP: f32 = 6.0;

/// A field label in the editor's label column.
fn form_label(ui: &mut Ui, text: &str) {
    ui.allocate_ui_with_layout(
        egui::vec2(FORM_LABEL_WIDTH, ui.spacing().interact_size.y),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(FORM_LABEL_WIDTH);
            ui.add(egui::Label::new(field_label(text)).truncate());
        },
    );
}

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
    /// Fires one shortcut (`shortcut_fire`): an operator control. The
    /// console's selected input rides along in `targets` (RC-004), and an
    /// OSC control's value in `value` (RC-004 dynamic values).
    Fire {
        shortcut: u64,
        targets: fm_protocol::WireTargetBindings,
        value: Option<fm_protocol::FireValue>,
    },
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
/// keys) outside the key profiles, as [`crate::keymap`]'s binding table
/// lists them (a unit test there keeps the two in step), then the keys
/// that move and press the keyboard focus. The active profile's keys
/// ([`crate::console_keymap`]) are reserved besides these.
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
    reserved(Key::T, NONE, false, "T"),
    reserved(Key::PageUp, NONE, false, "PgUp / PgDn"),
    reserved(Key::PageDown, NONE, false, "PgUp / PgDn"),
    reserved(Key::End, NONE, false, "End"),
    reserved(Key::Escape, NONE, false, "Esc"),
    reserved(Key::Comma, NONE, false, ","),
    reserved(Key::Period, NONE, false, "."),
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

/// The fixed console key a press of `named` with `pressed` modifiers
/// reaches, by its reference text.
fn fixed_key(named: Key, pressed: Modifiers) -> Option<&'static ReservedKey> {
    RESERVED_KEYS.iter().find(|reserved| {
        reserved.key == named
            && if reserved.logical {
                pressed.matches_logically(reserved.modifiers)
            } else {
                pressed.matches_exact(reserved.modifiers)
            }
    })
}

/// The console key a shortcut key would shadow, by its reference text: a
/// fixed console key, or a key of the active key profile `keymap`.
#[must_use]
pub(crate) fn reserved_by(key: &WireShortcutKey, keymap: &ConsoleKeymap) -> Option<String> {
    let named = Key::from_name(&key.key)?;
    let pressed = press(key);
    if let Some(reserved) = fixed_key(named, pressed) {
        return Some(reserved.keys.to_owned());
    }
    keymap
        .action(named, pressed)
        .and_then(|action| keymap.stroke(action))
        .map(ConsoleStroke::text)
}

/// The fixed console key (outside every key profile) a profile's `stroke`
/// would take, by its reference text. Space and Enter, which press the
/// focused control, are free for profiles: they reach a profile's action
/// only while no control has focus.
#[must_use]
pub(crate) fn fixed_console_key(stroke: ConsoleStroke) -> Option<&'static str> {
    if stroke.presses_focus() {
        return None;
    }
    let pressed = Modifiers {
        ctrl: stroke.command,
        ..stroke.modifiers()
    };
    fixed_key(stroke.key, pressed).map(|reserved| reserved.keys)
}

/// `Ctrl+Shift+Alt+F9`, or a chord `Ctrl+K, 1`: a shortcut key's strokes
/// as the keyboard reference writes a combination (see [`Locale::keys`] for
/// the console's words); its scope is shown apart.
#[must_use]
pub fn key_text(key: &WireShortcutKey) -> String {
    let mut text = stroke_text(key);
    if let Some(then) = &key.then {
        text.push_str(", ");
        text.push_str(&stroke_text(then));
    }
    text
}

/// One stroke of a key: `Ctrl+Shift+Alt+F9`.
fn stroke_text(key: &WireShortcutKey) -> String {
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

/// Where a scoped key answers: one console panel, that panel's pop-out
/// window, or one of the tool windows (Colour, Animation, Designer).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AnswerScope {
    Panel(crate::dock::PanelId),
    Window(crate::dock::PanelId),
    Tool(crate::dock::ToolWindow),
}

impl AnswerScope {
    /// Every scope the editor offers: each panel, then each panel's
    /// window, then each tool window.
    pub(crate) fn all() -> impl Iterator<Item = Self> {
        crate::dock::PanelId::ALL
            .into_iter()
            .map(Self::Panel)
            .chain(crate::dock::PanelId::ALL.into_iter().map(Self::Window))
            .chain(crate::dock::ToolWindow::ALL.into_iter().map(Self::Tool))
    }

    /// The scope a key's `scope` names.
    pub(crate) fn of(scope: &str) -> Option<Self> {
        match scope.strip_suffix(fm_protocol::SHORTCUT_WINDOW_SUFFIX) {
            Some(window) => crate::dock::PanelId::from_key(window)
                .map(Self::Window)
                .or_else(|| {
                    crate::dock::ToolWindow::ALL
                        .into_iter()
                        .find(|tool| tool.key() == window)
                        .map(Self::Tool)
                }),
            None => crate::dock::PanelId::from_key(scope).map(Self::Panel),
        }
    }

    /// The key's `scope` text.
    pub(crate) fn key(self) -> String {
        let window = match self {
            Self::Panel(panel) => return panel.key().to_owned(),
            Self::Window(panel) => panel.key(),
            Self::Tool(tool) => tool.key(),
        };
        format!("{window}{}", fm_protocol::SHORTCUT_WINDOW_SUFFIX)
    }

    /// The scope as the editor's picker names it.
    fn label(self, l: Locale) -> String {
        match self {
            Self::Panel(panel) => l.text(panel.title()).to_owned(),
            Self::Window(panel) => l.format(
                Msg::UserShortcutScopeWindow,
                &[("panel", Arg::Str(l.text(panel.title())))],
            ),
            Self::Tool(tool) => l.format(
                Msg::UserShortcutScopeTool,
                &[("tool", Arg::Str(l.text(tool.title())))],
            ),
        }
    }

    /// The picker's hover help for `scope` (`None` answers anywhere):
    /// `general` for anywhere and a panel, else the window's own.
    const fn hover(scope: Option<Self>, general: Msg) -> Msg {
        match scope {
            Some(Self::Window(_)) => Msg::UserShortcutScopeWindowHover,
            Some(Self::Tool(_)) => Msg::UserShortcutScopeToolHover,
            Some(Self::Panel(_)) | None => general,
        }
    }

    /// The name the key's words give the scope's panel or window.
    fn words(self, l: Locale) -> (Msg, &'static str) {
        match self {
            Self::Panel(panel) => (Msg::UserShortcutInPanel, l.text(panel.title())),
            Self::Window(panel) => (Msg::UserShortcutInWindow, l.text(panel.title())),
            Self::Tool(tool) => (Msg::UserShortcutInWindow, l.text(tool.title())),
        }
    }
}

/// The shortcut key name of a mouse button a shortcut may take: the
/// middle button and the side buttons (egui's extra buttons 1 and 2, back
/// and forward). The primary and secondary buttons stay the console's.
pub(crate) const fn mouse_button_name(button: PointerButton) -> Option<&'static str> {
    match button {
        PointerButton::Middle => Some("MouseMiddle"),
        PointerButton::Extra1 => Some("MouseBack"),
        PointerButton::Extra2 => Some("MouseForward"),
        PointerButton::Primary | PointerButton::Secondary => None,
    }
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
pub(crate) const fn modifier_only(key: Key) -> bool {
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

/// Takes every key press, typed text, and press of a mouse button a
/// shortcut may take from `events` (nothing else reads them while a capture
/// is on) and returns what the first new press means, if any came.
pub(crate) fn capture(events: &mut Vec<Event>) -> Option<Captured> {
    let mut captured = None;
    events.retain(|event| match event {
        Event::PointerButton {
            button,
            pressed,
            modifiers,
            ..
        } => {
            let Some(name) = mouse_button_name(*button) else {
                return true;
            };
            if *pressed && captured.is_none() {
                captured = Some(captured_stroke(name, *modifiers));
            }
            false
        }
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
    if matches!(key, Key::Copy | Key::Cut | Key::Paste) {
        return Captured::Unsupported;
    }
    captured_stroke(key.name(), modifiers)
}

/// A captured key or mouse button `name` with `modifiers`.
fn captured_stroke(name: &str, modifiers: Modifiers) -> Captured {
    // macOS's Control key (Ctrl without Cmd) has no place in a shortcut,
    // which names Ctrl on Linux and Windows and Cmd on macOS.
    if modifiers.ctrl && !modifiers.command {
        return Captured::Unsupported;
    }
    let named = WireShortcutKey {
        key: name.to_owned(),
        command: modifiers.command,
        shift: modifiers.shift,
        alt: modifiers.alt,
        then: None,
        scope: None,
    };
    if fm_protocol::shortcut_key_is_valid(&named) {
        Captured::Key(named)
    } else {
        Captured::Unsupported
    }
}

/// The keys project shortcuts answer on this console: each shortcut's
/// console key (see [`crate::shortcut_keys`]) when this console gives it
/// one, else its project key unless another shortcut's console key took
/// that key here.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConsoleKeys<'a> {
    pub(crate) shortcuts: &'a [ShortcutStatus],
    pub(crate) overrides: &'a ShortcutKeyOverrides,
    /// The project the overrides are looked up under (none before a
    /// snapshot, when no override applies).
    pub(crate) project: Option<ProjectId>,
    /// The active key profile's keys, which no shortcut key shadows.
    pub(crate) keymap: &'a ConsoleKeymap,
}

impl<'a> ConsoleKeys<'a> {
    /// The replicated shortcuts under `overrides`.
    pub(crate) fn of(state: &'a StudioUiState, overrides: &'a ShortcutKeyOverrides) -> Self {
        Self {
            shortcuts: replicated(state),
            overrides,
            project: state.view.as_ref().map(|view| view.cursor.project_id),
            keymap: state.console_keymap(),
        }
    }

    /// This console's override of `shortcut`: `None` without one,
    /// `Some(&None)` for no key here.
    pub(crate) fn local(self, shortcut: u64) -> Option<&'a Option<WireShortcutKey>> {
        self.overrides.get(self.project?, shortcut)
    }

    /// The other shortcut whose console key clashes with `key`, if one's
    /// does.
    pub(crate) fn local_owner(
        self,
        shortcut: u64,
        key: &WireShortcutKey,
    ) -> Option<&'a ShortcutStatus> {
        self.shortcuts.iter().find(|other| {
            other.id != shortcut
                && matches!(self.local(other.id), Some(Some(found)) if found.clashes(key))
        })
    }

    /// The key `shortcut` answers here and whether it is this console's
    /// own, or `None` when it answers no key here.
    pub(crate) fn key(self, shortcut: &'a ShortcutStatus) -> Option<(&'a WireShortcutKey, bool)> {
        if let Some(local) = self.local(shortcut.id) {
            return local.as_ref().map(|key| (key, true));
        }
        let key = shortcut.spec.key.as_ref()?;
        self.local_owner(shortcut.id, key)
            .is_none()
            .then_some((key, false))
    }

    /// The other shortcut whose key here clashes with `key`, if one does.
    pub(crate) fn answering(
        self,
        shortcut: u64,
        key: &WireShortcutKey,
    ) -> Option<&'a ShortcutStatus> {
        self.shortcuts.iter().find(|other| {
            other.id != shortcut && self.key(other).is_some_and(|(found, _)| found.clashes(key))
        })
    }

    /// The keys here a press can start, with their shortcuts: answering
    /// where `focus` is and not shadowing a console key.
    fn live(
        self,
        focus: KeyFocus<'a>,
    ) -> impl Iterator<Item = (&'a ShortcutStatus, &'a WireShortcutKey)> {
        self.shortcuts.iter().filter_map(move |shortcut| {
            let (key, _) = self.key(shortcut)?;
            (focus.answers(key.scope.as_deref()) && reserved_by(key, self.keymap).is_none())
                .then_some((shortcut, key))
        })
    }
}

/// Where a press lands: the panel with the console's focus, and the panel
/// or tool ([`fm_protocol::SHORTCUT_TOOL_WINDOWS`]) whose window has it
/// (`None` in the main window itself).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct KeyFocus<'a> {
    pub(crate) panel: Option<&'a str>,
    pub(crate) window: Option<&'a str>,
}

/// A window inside the console's main window that holds the focus scoped
/// keys answer in (RC-004): a pop-out the platform embeds in the main
/// window (it has a single viewport), or a tool window (Colour, Animation,
/// Designer) floating over the console, docked, or embedded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyWindow {
    Panel(crate::dock::PanelId),
    Tool(crate::dock::ToolWindow),
}

impl KeyFocus<'_> {
    /// Whether a key scoped to `scope` answers here: unscoped, scoped to
    /// the focused panel, or scoped to the window that has the press.
    fn answers(self, scope: Option<&str>) -> bool {
        let Some(scope) = scope else {
            return true;
        };
        match scope.strip_suffix(fm_protocol::SHORTCUT_WINDOW_SUFFIX) {
            Some(window) => self.window == Some(window),
            None => self.panel == Some(scope),
        }
    }
}

/// How long a chord waits for its second key, in seconds.
pub(crate) const CHORD_TIMEOUT: f64 = 2.0;

/// A chord's first key, pressed and waiting for its second.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingChord {
    /// The first stroke (no second stroke or scope).
    pub(crate) first: WireShortcutKey,
    /// When it was pressed (egui's input time).
    pub(crate) since: f64,
}

// The shell compares its state whole; an input time is never NaN.
impl Eq for PendingChord {}

/// One press a shortcut stroke may name: a key, or a named mouse button.
#[derive(Clone, Copy)]
enum Press {
    Key(Key),
    Mouse(&'static str),
}

impl Press {
    /// Whether this press with `modifiers` is `stroke`, matched exactly.
    fn is(self, stroke: &WireShortcutKey, modifiers: Modifiers) -> bool {
        let named = match self {
            Self::Key(key) => Key::from_name(&stroke.key) == Some(key),
            Self::Mouse(name) => stroke.key == name,
        };
        named && modifiers.matches_exact(pattern(stroke))
    }
}

/// Removes this frame's presses of shortcut keys and mouse buttons from
/// `events`, so nothing else reads them, and returns the shortcuts the new
/// presses name, in order. A named mouse button (middle, back, forward) is
/// a stroke like a key. A stroke that starts a chord waits for its second
/// stroke in `chord` (up to [`CHORD_TIMEOUT`] after it, at `now`); the next
/// key or named-button press then ends the chord, firing the shortcut it
/// completes, if any (any other press, Escape say, cancels the wait), and
/// is never read by anything else; a held key's repeats neither end nor
/// cancel it. Primary and
/// secondary clicks and other pointer events stay.
/// Keys scoped to a panel answer only while `focus` has that panel, and
/// keys scoped to a pop-out window only in that window. The repeats of a
/// held key are swallowed without firing again.
/// With `chord_end_only`, only the presses up to and including the one
/// that ends a pending chord are taken (none without one), so a chord's
/// second stroke is taken before the console keys read it.
fn take_presses_from(
    events: &mut Vec<Event>,
    keys: ConsoleKeys<'_>,
    focus: KeyFocus<'_>,
    chord: &mut Option<PendingChord>,
    now: f64,
    chord_end_only: bool,
) -> Vec<u64> {
    let mut fired = Vec::new();
    if chord
        .as_ref()
        .is_some_and(|pending| now - pending.since > CHORD_TIMEOUT)
    {
        *chord = None;
    }
    events.retain(|event| {
        let (press, repeat, modifiers) = match event {
            Event::PointerButton {
                button,
                pressed: true,
                modifiers,
                ..
            } => match mouse_button_name(*button) {
                Some(name) => (Press::Mouse(name), false, *modifiers),
                None => return true,
            },
            Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } if !modifier_only(*key) => (Press::Key(*key), *repeat, *modifiers),
            _ => return true,
        };
        let matches = |stroke: &WireShortcutKey| press.is(stroke, modifiers);
        if chord.is_some() && repeat {
            // A held key's repeat is not a new press: it neither ends nor
            // cancels the wait (a key held from before the chord would
            // otherwise drop it), and nothing else reads it meanwhile.
            return false;
        }
        if let Some(pending) = chord.as_ref() {
            // A chord's second stroke, a key or a mouse button, ends it.
            let completed = keys.live(focus).find(|(_, bound)| {
                bound.same_stroke(&pending.first) && bound.then.as_deref().is_some_and(matches)
            });
            // Any other new press (Escape, say) cancels the wait, swallowed
            // and firing nothing.
            if let Some((shortcut, _)) = completed {
                fired.push(shortcut.id);
            }
            *chord = None;
            return false;
        }
        if chord_end_only {
            return true;
        }
        let mut starts = keys.live(focus).filter(|(_, bound)| matches(bound));
        let Some((shortcut, bound)) = starts.next() else {
            return true;
        };
        if !repeat {
            if bound.then.is_some() {
                *chord = Some(PendingChord {
                    first: WireShortcutKey::stroke(
                        bound.key.clone(),
                        bound.command,
                        bound.shift,
                        bound.alt,
                    ),
                    since: now,
                });
            } else {
                fired.push(shortcut.id);
            }
        }
        false
    });
    fired
}

/// [`take_presses_from`] taking every press.
#[cfg(test)]
pub(crate) fn take_presses(
    events: &mut Vec<Event>,
    keys: ConsoleKeys<'_>,
    focus: KeyFocus<'_>,
    chord: &mut Option<PendingChord>,
    now: f64,
) -> Vec<u64> {
    take_presses_from(events, keys, focus, chord, now, false)
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

/// Why a fire of `shortcut` carrying `value` cannot run, if it cannot: an
/// action taking its gain from the firing control needs a value, which
/// only an OSC message sends (RC-004 dynamic values).
#[must_use]
pub fn value_block(
    shortcut: &ShortcutStatus,
    value: Option<fm_protocol::FireValue>,
) -> Option<&'static str> {
    let WireShortcutAction::Actions(actions) = &shortcut.spec.action else {
        return None;
    };
    (value.is_none() && actions.iter().any(reads_fire_value))
        .then(|| Msg::UserShortcutReasonNeedsValue.en())
}

/// Whether `action` takes its value from the firing control.
fn reads_fire_value(action: &WireCueAction) -> bool {
    matches!(
        action,
        WireCueAction::AudioGain {
            gain: fm_protocol::CueGain::Fired { .. },
            ..
        }
    )
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
    shortcuts.iter().find(|other| {
        other.id != shortcut
            && other
                .spec
                .key
                .as_ref()
                .is_some_and(|other| other.clashes(key))
    })
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
            .is_some_and(|key| reserved_by(key, state.console_keymap()).is_some())
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
// Shortcut set files
// ---------------------------------------------------------------------------

/// Why a shortcut set cannot be imported here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportRefusal {
    /// The session cannot edit the project (a reason's English text).
    Blocked(&'static str),
    /// The merged set would break a bound or clash.
    Plan(ShortcutImportError),
    /// Shortcut `name` would take `keys`, one of the console's own keys.
    Reserved { name: String, keys: String },
}

impl ImportRefusal {
    /// The refusal in `l`'s words.
    #[must_use]
    pub fn text(&self, l: Locale) -> String {
        match self {
            Self::Blocked(reason) => l.tr(reason).to_owned(),
            Self::Plan(ShortcutImportError::TooMany { total }) => l.format(
                Msg::UserShortcutImportTooMany,
                &[("count", Arg::Plain(*total as u64))],
            ),
            Self::Plan(ShortcutImportError::KeyTaken { imported, existing }) => l.format(
                Msg::UserShortcutImportKeyTaken,
                &[
                    ("imported", Arg::Str(imported)),
                    ("existing", Arg::Str(existing)),
                ],
            ),
            Self::Plan(ShortcutImportError::OscTaken { imported, existing }) => l.format(
                Msg::UserShortcutImportOscTaken,
                &[
                    ("imported", Arg::Str(imported)),
                    ("existing", Arg::Str(existing)),
                ],
            ),
            Self::Plan(ShortcutImportError::Swap { first, second }) => l.format(
                Msg::UserShortcutImportSwap,
                &[("first", Arg::Str(first)), ("second", Arg::Str(second))],
            ),
            Self::Plan(ShortcutImportError::NoFreeId) => {
                l.text(Msg::UserShortcutImportNoId).to_owned()
            }
            Self::Reserved { name, keys } => l.format(
                Msg::UserShortcutImportReserved,
                &[("name", Arg::Str(name)), ("keys", Arg::Str(&l.keys(keys)))],
            ),
        }
    }
}

/// The `shortcut_set` edits that merge `imported` (a decoded `.fmshortcuts`
/// set) into the replicated project, or why it cannot be merged: the
/// session's edit gate, a key the console keeps for itself, then the
/// merge's own bounds and clashes (see
/// [`fm_preset_file::shortcuts::plan_shortcut_import`]).
///
/// # Errors
///
/// [`ImportRefusal`] when nothing may be imported.
pub fn import_plan(
    state: &StudioUiState,
    imported: &[ShortcutSpec],
) -> Result<ShortcutImportPlan, ImportRefusal> {
    if let Some(reason) = crate::cues::edit_block(state) {
        return Err(ImportRefusal::Blocked(reason));
    }
    if let Some((name, keys)) = imported.iter().find_map(|spec| {
        let keys = reserved_by(spec.key.as_ref()?, state.console_keymap())?;
        Some((spec.name.clone(), keys))
    }) {
        return Err(ImportRefusal::Reserved { name, keys });
    }
    plan_shortcut_import(replicated(state), imported).map_err(ImportRefusal::Plan)
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// The window's open state and the draft being edited.
#[allow(clippy::struct_excessive_bools)] // the open window and its two captures
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
    /// Whether that capture binds the key's second stroke (a chord).
    pub(crate) capturing_then: bool,
    /// The last capture pressed a key a shortcut cannot name.
    unsupported: bool,
    /// Whether the next key press binds the selected shortcut's key on
    /// this console.
    pub(crate) capturing_local: bool,
    /// Whether that capture binds the console key's second stroke (a
    /// chord).
    pub(crate) capturing_local_then: bool,
    /// Why the last console key capture bound nothing.
    pub(crate) local_refusal: Option<LocalRefusal>,
    /// The `.fmshortcuts` path typed in the set file row.
    pub(crate) file_path: String,
    /// Set exports and imports waiting for the host.
    pub(crate) file_requests: Vec<PresetFileRequest>,
}

impl ShortcutsWindow {
    /// Edits `shortcut` (or nothing when the project has none).
    pub(crate) fn select(&mut self, shortcuts: &[ShortcutStatus], shortcut: Option<u64>) {
        let found = shortcut.and_then(|id| shortcuts.iter().find(|found| found.id == id));
        self.selected = found.map(|found| found.id);
        self.draft = found.map(|found| found.spec.clone());
        self.loaded.clone_from(&self.draft);
        self.capturing = false;
        self.capturing_then = false;
        self.unsupported = false;
        self.capturing_local = false;
        self.capturing_local_then = false;
        self.local_refusal = None;
    }

    /// Starts a new draft under the next free id (the lowest free one once
    /// the largest id is taken): cue actions, none yet.
    pub(crate) fn new_draft(&mut self, l: Locale, shortcuts: &[ShortcutStatus]) {
        let taken = |id: u64| shortcuts.iter().any(|found| found.id == id);
        let next = shortcuts
            .iter()
            .map(|found| found.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .or_else(|| (1..=u64::MAX).find(|id| !taken(*id)))
            .unwrap_or(1);
        self.selected = Some(next);
        self.draft = Some(ShortcutSpec {
            name: l.format(Msg::IdKindShortcut, &[("id", Arg::Plain(next))]),
            enabled: true,
            key: None,
            osc: None,
            action: WireShortcutAction::Actions(Vec::new()),
        });
        self.loaded = None;
        self.capturing = false;
        self.capturing_then = false;
        self.unsupported = false;
        self.capturing_local = false;
        self.capturing_local_then = false;
        self.local_refusal = None;
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
                let (capturing, capturing_then) = (self.capturing, self.capturing_then);
                let (capturing_local, capturing_local_then) =
                    (self.capturing_local, self.capturing_local_then);
                self.select(shortcuts, Some(id));
                self.capturing = capturing;
                self.capturing_then = capturing_then;
                self.capturing_local = capturing_local;
                self.capturing_local_then = capturing_local_then;
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
                Some(Captured::Key(stroke)) => {
                    if let Some(draft) = &mut window.draft {
                        draft.key = Some(match (draft.key.take(), window.capturing_then) {
                            // The second stroke of a chord.
                            (Some(mut key), true) => {
                                key.then = Some(Box::new(stroke));
                                key
                            }
                            // A new first stroke keeps the second and the
                            // scope.
                            (Some(key), false) => WireShortcutKey {
                                then: key.then,
                                scope: key.scope,
                                ..stroke
                            },
                            (None, _) => stroke,
                        });
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
        if window.capturing_local {
            let captured = context.input_mut(|input| capture(&mut input.events));
            let keys = ConsoleKeys::of(state, &self.shortcut_keys);
            if let Some(captured) = captured {
                window.capturing_local = false;
                let then = core::mem::take(&mut window.capturing_local_then);
                let bound = window
                    .selected
                    .zip(keys.project)
                    .and_then(|(shortcut, project)| {
                        let captured = local_stroke(keys.local(shortcut), captured, then);
                        local_capture(keys, shortcut, captured).map(|key| (project, shortcut, key))
                    });
                match bound {
                    Some((project, shortcut, Ok(key))) => {
                        window.local_refusal =
                            (!self.shortcut_keys.set(project, shortcut, Some(key)))
                                .then_some(LocalRefusal::Limit);
                    }
                    Some((_, _, Err(refusal))) => window.local_refusal = Some(refusal),
                    None => {}
                }
            }
        }
        let overrides = &mut self.shortcut_keys;
        let close = crate::menu::help_modal(
            context,
            "studio-user-shortcuts",
            l.text(Msg::UserShortcutsTitle),
            DIALOG_WIDTH,
            |ui| draw_contents(ui, state, window, overrides, intents),
        );
        if close && !self.user_shortcuts.capturing && !self.user_shortcuts.capturing_local {
            self.user_shortcuts.open = false;
        }
    }

    /// Fires the shortcuts this frame's key and mouse button presses name,
    /// under the console keys' gates (no dialog, menu, keyboard-owning
    /// panel, or text field has the keyboard); a fire this session cannot
    /// send is announced. `popout` is the panel whose pop-out window `ui`
    /// draws (its presses answer that panel and that window), or `None` in
    /// the main window (the focused panel's).
    pub(crate) fn fire_user_shortcut_keys(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        popout: Option<crate::dock::PanelId>,
        intents: &mut Vec<StudioIntent>,
    ) {
        self.fire_user_keys(ui, state, popout, intents, false);
    }

    /// Ends a pending chord with this frame's next press, firing the
    /// shortcut it completes, before the console keys read that press: a
    /// chord's second stroke may be a console key alone (RC-004).
    pub(crate) fn end_user_shortcut_chord(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        popout: Option<crate::dock::PanelId>,
        intents: &mut Vec<StudioIntent>,
    ) {
        if self.chord.is_some() {
            self.fire_user_keys(ui, state, popout, intents, true);
        }
    }

    /// [`Self::fire_user_shortcut_keys`], or with `chord_end_only` only up
    /// to the press that ends a pending chord.
    fn fire_user_keys(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        popout: Option<crate::dock::PanelId>,
        intents: &mut Vec<StudioIntent>,
        chord_end_only: bool,
    ) {
        if popout.is_none()
            && let Some(KeyWindow::Tool(tool)) = self.main_window(ui.ctx())
            && self.tool_owns_keyboard(tool)
        {
            // The focused tool reads them after its own keys took theirs
            // (and keeps a chord it started).
            return;
        }
        if self.console_keys_blocked(ui.ctx()) || ui.ctx().egui_wants_keyboard_input() {
            // A text field or dialog took the keyboard: the wait ends, so
            // the first key after it closes is not swallowed as a chord's
            // second stroke.
            self.chord = None;
            return;
        }
        let focus = match popout {
            Some(panel) => KeyFocus {
                panel: Some(panel.key()),
                window: Some(panel.key()),
            },
            None => self.main_key_focus(ui.ctx()),
        };
        self.fire_keys_at(ui, state, focus, intents, chord_end_only);
    }

    /// The window inside the main window with the focus, if it is still
    /// there: an embedded pop-out, or an open tool not in a native window.
    fn main_window(&self, context: &egui::Context) -> Option<KeyWindow> {
        self.focused_window.filter(|window| match *window {
            KeyWindow::Panel(panel) => {
                context.embed_viewports()
                    && self
                        .layout
                        .dock
                        .popped()
                        .iter()
                        .any(|popout| popout.key == panel)
            }
            KeyWindow::Tool(tool) => {
                self.tool_open(tool)
                    && (self.layout.tool_popped(tool).is_none() || context.embed_viewports())
            }
        })
    }

    /// Where the main window's presses land: an embedded pop-out or a tool
    /// window there with the focus (the window last clicked), else the
    /// focused panel.
    fn main_key_focus(&self, context: &egui::Context) -> KeyFocus<'static> {
        match self.main_window(context) {
            Some(KeyWindow::Panel(panel)) => KeyFocus {
                panel: Some(panel.key()),
                window: Some(panel.key()),
            },
            Some(KeyWindow::Tool(tool)) => KeyFocus {
                panel: None,
                window: Some(tool.key()),
            },
            None => KeyFocus {
                panel: self.focused_panel.map(crate::dock::PanelId::key),
                window: None,
            },
        }
    }

    /// Whether tool window `tool` is open.
    const fn tool_open(&self, tool: crate::dock::ToolWindow) -> bool {
        match tool {
            crate::dock::ToolWindow::Color => self.color.open,
            crate::dock::ToolWindow::Animation => self.animation.open,
            crate::dock::ToolWindow::Designer => self.designer.open,
        }
    }

    /// Whether tool window `tool` owns the keyboard for its own keys.
    fn tool_owns_keyboard(&self, tool: crate::dock::ToolWindow) -> bool {
        match tool {
            crate::dock::ToolWindow::Color => self.color_owns_keyboard(),
            crate::dock::ToolWindow::Animation => self.animation_owns_keyboard(),
            crate::dock::ToolWindow::Designer => self.designer_owns_keyboard(),
        }
    }

    /// Gives `window` the focus scoped keys answer in when this frame's
    /// press landed in `ui` (on its layer, and inside `area` when given:
    /// a docked tool shares the console's layer). Call after drawing a
    /// window that shares the main window's input.
    pub(crate) fn focus_window_on_press(
        &mut self,
        ui: &Ui,
        area: Option<egui::Rect>,
        window: KeyWindow,
    ) {
        let pressed = ui.input(|input| {
            input.pointer.interact_pos().filter(|pos| {
                input.pointer.any_pressed() && area.is_none_or(|area| area.contains(*pos))
            })
        });
        // Outside the input lock: the layer lookup takes the memory's.
        if pressed.is_some_and(|pos| ui.ctx().layer_id_at(pos) == Some(ui.layer_id())) {
            self.focused_window = Some(window);
        }
    }

    /// Reads the project's shortcut keys and mouse buttons in tool window
    /// `tool` after its own keys took theirs (RC-004): its window-scoped
    /// keys and the console's unscoped ones. `native` is a native window
    /// with its own input, which always reads them; a tool sharing the main
    /// window's input (floating, docked in `docked` on the console's layer,
    /// or embedded) gives the focus to itself on a press, and reads here
    /// only while it has the focus and owns the keyboard (the main window
    /// read the keys otherwise).
    pub(crate) fn tool_window_keys(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        tool: crate::dock::ToolWindow,
        (native, docked): (bool, Option<egui::Rect>),
        intents: &mut Vec<StudioIntent>,
    ) {
        if !native {
            self.focus_window_on_press(ui, docked, KeyWindow::Tool(tool));
            if self.focused_window != Some(KeyWindow::Tool(tool)) || !self.tool_owns_keyboard(tool)
            {
                return;
            }
        }
        // The designer's canvas holds egui's focus while it owns the keys.
        let field = ui.ctx().egui_wants_keyboard_input()
            && !(tool == crate::dock::ToolWindow::Designer && self.designer_owns_keyboard());
        if field || self.window_keys_blocked() || egui::Popup::is_any_open(ui.ctx()) {
            return;
        }
        let focus = KeyFocus {
            panel: None,
            window: Some(tool.key()),
        };
        self.fire_keys_at(ui, state, focus, intents, false);
    }

    /// Fires the shortcuts this frame's presses name where `focus` is.
    fn fire_keys_at(
        &mut self,
        ui: &mut Ui,
        state: &StudioUiState,
        focus: KeyFocus<'_>,
        intents: &mut Vec<StudioIntent>,
        chord_end_only: bool,
    ) {
        let keys = ConsoleKeys::of(state, &self.shortcut_keys);
        if keys.shortcuts.is_empty() {
            self.chord = None;
            return;
        }
        let chord = &mut self.chord;
        let fired = ui.input_mut(|input| {
            let now = input.time;
            take_presses_from(&mut input.events, keys, focus, chord, now, chord_end_only)
        });
        if let Some(pending) = self.chord.as_ref().filter(|_| !chord_end_only) {
            let l = self.locale;
            let waiting = l.format(
                Msg::UserShortcutChordPending,
                &[("keys", Arg::Str(&l.keys(&key_text(&pending.first))))],
            );
            egui::Area::new(egui::Id::new("studio-user-shortcut-chord"))
                .order(egui::Order::Tooltip)
                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -48.0))
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label(RichText::new(waiting).color(accent_color()));
                    });
                });
            let left = CHORD_TIMEOUT - (ui.input(|input| input.time) - pending.since);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(left.max(0.0) + 0.05));
        }
        for shortcut in fired {
            if let Some(intent) = self.user_shortcut_intent(state, shortcut, None) {
                intents.push(intent);
            }
        }
    }

    /// The intent that fires `shortcut` with the firing control's `value`,
    /// or `None` after announcing why this session cannot.
    fn user_shortcut_intent(
        &mut self,
        state: &StudioUiState,
        shortcut: u64,
        value: Option<fm_protocol::FireValue>,
    ) -> Option<StudioIntent> {
        let found = replicated(state)
            .iter()
            .find(|found| found.id == shortcut)?;
        match fire_block(state, found).or_else(|| value_block(found, value)) {
            None => Some(StudioIntent::Shortcut(ShortcutIntent::Fire {
                shortcut,
                targets: fm_protocol::WireTargetBindings::default(),
                value,
            })),
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
    /// The fire carries this console's selected input, as a key's does
    /// (RC-004): it is sent outside [`StudioShell::draw`], which binds it.
    /// `value` is the message's float argument, the control's position (an
    /// action taking its gain from the firing control needs one).
    pub fn user_shortcut_osc_intent(
        &mut self,
        state: &StudioUiState,
        name: &str,
        value: Option<fm_protocol::FireValue>,
    ) -> Option<StudioIntent> {
        let shortcut = by_osc(replicated(state), name)?.id;
        let mut intent = self.user_shortcut_intent(state, shortcut, value)?;
        crate::selection::bind(self.selected_input, core::slice::from_mut(&mut intent));
        Some(intent)
    }
}

fn draw_contents(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut ShortcutsWindow,
    overrides: &mut ShortcutKeyOverrides,
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
            draw_list(ui, state, window, ConsoleKeys::of(state, overrides));
        });
        let (gap, _) = ui.allocate_exact_size(egui::vec2(DIVIDER_GAP, 0.0), egui::Sense::hover());
        ui.vertical(|ui| {
            if window.draft.is_some() {
                draw_editor(ui, state, window, shortcuts, intents);
                draw_console_key(ui, state, window, overrides);
            } else {
                ui.set_min_width(ui.available_width());
                empty_state(
                    ui,
                    l.text(Msg::UserShortcutsEmpty),
                    l.text(Msg::UserShortcutsEmptyDetail),
                );
            }
        });
        gap.center().x
    });
    ui.painter().vline(
        row.inner,
        row.response.rect.y_range(),
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
    ui.separator();
    draw_file_row(ui, state, window, shortcuts);
}

/// Sharing the shortcuts through a `.fmshortcuts` file: export them all,
/// or import a set into the project.
fn draw_file_row(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut ShortcutsWindow,
    shortcuts: &[ShortcutStatus],
) {
    let l = l10n::locale(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(section_label(l.text(Msg::UserShortcutFile)))
            .on_hover_text(l.text(Msg::UserShortcutFileHover));
        ui.add(
            TextEdit::singleline(&mut window.file_path)
                .hint_text(l.text(Msg::UserShortcutPathHint))
                .desired_width(300.0),
        )
        .on_hover_text(l.text(Msg::UserShortcutPathHover));
        let export = export_block(&window.file_path, shortcuts);
        if ui
            .add_enabled(
                export.is_none(),
                small_button(l.text(Msg::UserShortcutExport)),
            )
            .hint(l.text(Msg::UserShortcutExportHover), export.unwrap_or(""))
            .clicked()
        {
            window.export_set(state, shortcuts);
        }
        let import = import_block(state, &window.file_path);
        if ui
            .add_enabled(
                import.is_none(),
                small_button(l.text(Msg::UserShortcutImport)),
            )
            .hint(l.text(Msg::UserShortcutImportHover), import.unwrap_or(""))
            .clicked()
        {
            window.import_set();
        }
    });
}

/// Why the shortcuts cannot be exported to `path`, if they cannot: no
/// path, then no shortcuts. Exporting is not an edit, so a viewer may.
fn export_block(path: &str, shortcuts: &[ShortcutStatus]) -> Option<&'static str> {
    if path.trim().is_empty() {
        Some(Msg::AnimReasonNoPath.en())
    } else if shortcuts.is_empty() {
        Some(Msg::UserShortcutReasonNoShortcuts.en())
    } else {
        None
    }
}

/// Why a set cannot be imported from `path`, if it cannot: the edit gate,
/// then no path.
fn import_block(state: &StudioUiState, path: &str) -> Option<&'static str> {
    crate::cues::edit_block(state)
        .or_else(|| path.trim().is_empty().then_some(Msg::AnimReasonNoPath.en()))
}

impl ShortcutsWindow {
    /// Asks the host to write `shortcuts` to the typed path, named after
    /// the show (or `Shortcuts`).
    pub(crate) fn export_set(&mut self, state: &StudioUiState, shortcuts: &[ShortcutStatus]) {
        let name = state
            .project_state
            .as_ref()
            .map(fm_ui_model::ProjectState::show_name)
            .map(str::trim)
            .filter(|name| {
                !name.is_empty()
                    && name.len() <= fm_preset_file::shortcuts::MAX_SHORTCUT_SET_NAME_BYTES
            })
            .unwrap_or("Shortcuts")
            .to_owned();
        self.file_requests.push(PresetFileRequest::ExportShortcuts {
            path: self.file_path.trim().into(),
            name,
            shortcuts: shortcuts.iter().map(|found| found.spec.clone()).collect(),
        });
    }

    /// Asks the host to read the set at the typed path.
    pub(crate) fn import_set(&mut self) {
        self.file_requests.push(PresetFileRequest::ImportShortcuts {
            path: self.file_path.trim().into(),
        });
    }
}

/// A list row's text: the name, then the key it answers on this console
/// in the console's words (marked when it is this console's own).
fn row_text(l: Locale, keys: ConsoleKeys<'_>, shortcut: &ShortcutStatus) -> String {
    let mut text = shortcut.spec.name.clone();
    if let Some((key, local)) = keys.key(shortcut) {
        text.push_str("  ");
        text.push_str(&key_words(l, key, local));
    }
    if !shortcut.spec.enabled {
        text = l.format(Msg::UserShortcutListOff, &[("name", Arg::Str(&text))]);
    }
    text
}

/// A picker row on two lines: the name (dim with `(off)` while the
/// shortcut is off), then its key in the console's words, so a long chord
/// or panel never wraps into the name.
fn row_job(l: Locale, keys: ConsoleKeys<'_>, shortcut: &ShortcutStatus) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let (name, name_color) = if shortcut.spec.enabled {
        (shortcut.spec.name.clone(), text_color())
    } else {
        (
            l.format(
                Msg::UserShortcutListOff,
                &[("name", Arg::Str(&shortcut.spec.name))],
            ),
            muted_color(),
        )
    };
    job.append(
        &name,
        0.0,
        egui::TextFormat::simple(
            egui::FontId::proportional(crate::theme::legible(12.5)),
            name_color,
        ),
    );
    let key = keys.key(shortcut).map_or_else(
        || l.text(Msg::UserShortcutKeyNone).to_owned(),
        |(key, local)| key_words(l, key, local),
    );
    job.append(
        &format!("\n{key}"),
        0.0,
        egui::TextFormat::simple(
            egui::FontId::monospace(crate::theme::legible(10.0)),
            muted_color(),
        ),
    );
    job.wrap.max_width = LIST_WIDTH - 12.0;
    job
}

fn draw_list(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut ShortcutsWindow,
    keys: ConsoleKeys<'_>,
) {
    let shortcuts = keys.shortcuts;
    let l = l10n::locale(ui.ctx());
    ui.label(section_label(l.text(Msg::UserShortcutsTitle)));
    for shortcut in shortcuts {
        let selected = window.selected == Some(shortcut.id);
        let row = ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
            ui.add(Button::selectable(selected, row_job(l, keys, shortcut)))
        });
        // Screen readers hear the row on one line.
        row.inner.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Button,
                true,
                selected,
                row_text(l, keys, shortcut),
            )
        });
        if row
            .inner
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
        .add_enabled(
            block.is_none(),
            small_button(format!("+  {}", l.text(Msg::UserShortcutNew))),
        )
        .hint(l.text(Msg::UserShortcutNewHover), block.unwrap_or(""))
        .clicked()
    {
        window.new_draft(l, shortcuts);
    }
}

// ---------------------------------------------------------------------------
// This console's key
// ---------------------------------------------------------------------------

/// Why a console key capture bound nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LocalRefusal {
    /// A key a shortcut cannot name.
    Unsupported,
    /// One of the console's own keys, by its reference text.
    Reserved(String),
    /// The key another shortcut already answers here.
    Taken(String),
    /// The console keeps [`MAX_KEY_OVERRIDES`] overrides already.
    Limit,
}

/// A key in the console's words with the panel or window it answers in, marked
/// when it is this console's own.
fn key_words(l: Locale, key: &WireShortcutKey, local: bool) -> String {
    let mut keys = l.keys(&key_text(key));
    if let Some(scope) = key.scope.as_deref().and_then(AnswerScope::of) {
        let (message, panel) = scope.words(l);
        keys = l.format(
            message,
            &[("keys", Arg::Str(&keys)), ("panel", Arg::Str(panel))],
        );
    }
    if local {
        l.format(Msg::UserShortcutListLocal, &[("keys", Arg::Str(&keys))])
    } else {
        keys
    }
}

/// What a console key capture for `shortcut` binds: the key, or why it
/// binds nothing; `None` for Esc (nothing changes).
pub(crate) fn local_capture(
    keys: ConsoleKeys<'_>,
    shortcut: u64,
    captured: Captured,
) -> Option<Result<WireShortcutKey, LocalRefusal>> {
    match captured {
        Captured::Cancel => None,
        Captured::Unsupported => Some(Err(LocalRefusal::Unsupported)),
        Captured::Key(key) => Some(if let Some(reserved) = reserved_by(&key, keys.keymap) {
            Err(LocalRefusal::Reserved(reserved))
        } else if let Some(other) = keys.answering(shortcut, &key) {
            Err(LocalRefusal::Taken(other.spec.name.clone()))
        } else {
            Ok(key)
        }),
    }
}

/// What a console key capture of `captured` makes of this console's key
/// `local` (see [`ConsoleKeys::local`]): with `then`, the second stroke of
/// its chord; else a new first stroke that keeps the key's second stroke
/// and its scope. Either stroke may be a mouse button.
pub(crate) fn local_stroke(
    local: Option<&Option<WireShortcutKey>>,
    captured: Captured,
    then: bool,
) -> Captured {
    let Captured::Key(stroke) = captured else {
        return captured;
    };
    let current = local.and_then(Option::as_ref);
    match (current, then) {
        (Some(key), true) => Captured::Key(WireShortcutKey {
            then: Some(Box::new(stroke)),
            ..key.clone()
        }),
        (_, true) => Captured::Unsupported,
        (Some(key), false) => Captured::Key(WireShortcutKey {
            then: key.then.clone(),
            scope: key.scope.clone(),
            ..stroke
        }),
        (None, false) => Captured::Key(stroke),
    }
}

/// Why this console cannot keep one more override for `shortcut`, if it
/// cannot.
fn override_block(keys: ConsoleKeys<'_>, shortcut: u64) -> Option<&'static str> {
    (keys.local(shortcut).is_none() && keys.overrides.len() >= MAX_KEY_OVERRIDES)
        .then_some(Msg::UserShortcutReasonOverrideLimit.en())
}

/// The "This console" row: the key the selected shortcut answers on this
/// console, Set console key (capture), Use project key, and No key here.
/// These change this console's preferences, not the project, so they need
/// no edit permission.
#[allow(clippy::too_many_lines)] // one immediate-mode row
fn draw_console_key(
    ui: &mut Ui,
    state: &StudioUiState,
    window: &mut ShortcutsWindow,
    overrides: &mut ShortcutKeyOverrides,
) {
    let l = l10n::locale(ui.ctx());
    let Some(shortcut) = window.selected else {
        return;
    };
    let keys = ConsoleKeys::of(state, overrides);
    let (Some(project), Some(found)) = (
        keys.project,
        keys.shortcuts.iter().find(|found| found.id == shortcut),
    ) else {
        return;
    };
    let local = keys.local(shortcut).cloned();
    let shadowed = found
        .spec
        .key
        .as_ref()
        .and_then(|key| keys.local_owner(shortcut, key))
        .filter(|_| local.is_none())
        .map(|other| other.spec.name.clone());
    let limit = override_block(keys, shortcut);
    let capturing = window.capturing || window.capturing_local;
    // Some(true): use the project key; Some(false): no key here.
    let mut use_project: Option<bool> = None;
    // This console's key changed by its chord or scope controls.
    let mut reshaped: Option<WireShortcutKey> = None;
    // The label holds its column; the status and keys wrap under each
    // other beside it (Spanish "Sin tecla aquí"), not back under the label.
    ui.horizontal(|ui| {
        form_label(ui, l.text(Msg::UserShortcutLocalLabel));
        ui.horizontal_wrapped(|ui| {
            match (&local, window.capturing_local) {
                (_, true) => ui.label(
                    RichText::new(l.text(if window.capturing_local_then {
                        Msg::UserShortcutLocalThenCapturing
                    } else {
                        Msg::UserShortcutLocalCapturing
                    }))
                    .color(accent_color()),
                ),
                (Some(Some(key)), false) => {
                    let response = crate::menu::key_cap(ui, &l.keys(&key_text(key)), 0.0);
                    if let Some(scope) = key.scope.as_deref().and_then(AnswerScope::of) {
                        ui.label(RichText::new(scope.label(l)).color(muted_color()))
                            .on_hover_text(l.text(Msg::UserShortcutLocalScopeHover));
                    }
                    response
                }
                (Some(None), false) => ui.label(
                    RichText::new(l.text(Msg::UserShortcutLocalNone))
                        .size(crate::theme::legible(11.0))
                        .color(muted_color()),
                ),
                (None, false) => ui.label(
                    RichText::new(l.text(Msg::UserShortcutLocalProject))
                        .size(crate::theme::legible(11.0))
                        .color(muted_color()),
                ),
            }
            .on_hover_text(l.text(Msg::UserShortcutLocalHover));
            ui.add_space(4.0);
            let set = if capturing {
                Some(Msg::UserShortcutReasonCapturing.en())
            } else {
                limit
            };
            if ui
                .add_enabled(
                    set.is_none(),
                    small_button(l.text(Msg::UserShortcutSetLocal)),
                )
                .hint(l.text(Msg::UserShortcutSetLocalHover), set.unwrap_or(""))
                .clicked()
            {
                window.capturing_local = true;
                window.capturing_local_then = false;
                window.local_refusal = None;
            }
            let project_key = local
                .is_none()
                .then_some(Msg::UserShortcutReasonProjectKeyHere.en());
            if ui
                .add_enabled(
                    project_key.is_none(),
                    small_button(l.text(Msg::UserShortcutUseProject)),
                )
                .hint(
                    l.text(Msg::UserShortcutUseProjectHover),
                    project_key.unwrap_or(""),
                )
                .clicked()
            {
                use_project = Some(true);
            }
            let no_key = (local == Some(None))
                .then_some(Msg::UserShortcutReasonNoKeyHere.en())
                .or(limit);
            if ui
                .add_enabled(
                    no_key.is_none(),
                    small_button(l.text(Msg::UserShortcutNoLocalKey)),
                )
                .hint(
                    l.text(Msg::UserShortcutNoLocalKeyHover),
                    no_key.unwrap_or(""),
                )
                .clicked()
            {
                use_project = Some(false);
            }
        });
    });
    // The console key's chord and the panel or window it answers in.
    let own = match &local {
        Some(Some(key)) => Some(key.clone()),
        _ => None,
    };
    let reshape_block = if capturing {
        Some(Msg::UserShortcutReasonCapturing.en())
    } else if own.is_none() {
        Some(Msg::UserShortcutReasonNoLocalKey.en())
    } else {
        None
    };
    ui.horizontal_wrapped(|ui| {
        form_label(ui, l.text(Msg::UserShortcutLocalChordLabel));
        if let Some(key) = own.as_ref().filter(|key| key.then.is_some()) {
            if ui
                .add_enabled(
                    reshape_block.is_none(),
                    small_button(l.text(Msg::UserShortcutLocalClearThen)),
                )
                .hint(
                    l.text(Msg::UserShortcutLocalClearThenHover),
                    reshape_block.unwrap_or(""),
                )
                .clicked()
            {
                reshaped = Some(WireShortcutKey {
                    then: None,
                    ..key.clone()
                });
            }
        } else {
            let add = reshape_block;
            if ui
                .add_enabled(
                    add.is_none(),
                    small_button(l.text(Msg::UserShortcutLocalSetThen)),
                )
                .hint(
                    l.text(Msg::UserShortcutLocalSetThenHover),
                    add.unwrap_or(""),
                )
                .clicked()
            {
                window.capturing_local = true;
                window.capturing_local_then = true;
                window.local_refusal = None;
            }
        }
        ui.add_space(8.0);
        ui.label(l.text(Msg::UserShortcutLocalScopeLabel))
            .on_hover_text(l.text(Msg::UserShortcutLocalScopeHover));
        let current = own
            .as_ref()
            .and_then(|key| key.scope.as_deref())
            .and_then(AnswerScope::of);
        let name = |scope: Option<AnswerScope>| {
            scope.map_or_else(
                || l.text(Msg::UserShortcutScopeAnywhere).to_owned(),
                |scope| scope.label(l),
            )
        };
        let mut chosen = current;
        ui.add_enabled_ui(reshape_block.is_none(), |ui| {
            ComboBox::from_id_salt("studio-user-shortcut-local-scope")
                .selected_text(name(current))
                .width(200.0)
                .show_ui(ui, |ui| {
                    for scope in core::iter::once(None).chain(AnswerScope::all().map(Some)) {
                        let hover = AnswerScope::hover(scope, Msg::UserShortcutLocalScopeHover);
                        ui.selectable_value(&mut chosen, scope, name(scope))
                            .on_hover_text(l.text(hover));
                    }
                })
                .response
                .hint(
                    l.text(Msg::UserShortcutLocalScopeHover),
                    reshape_block.unwrap_or(""),
                );
        });
        if chosen != current
            && let Some(key) = &own
        {
            reshaped = Some(WireShortcutKey {
                scope: chosen.map(AnswerScope::key),
                ..key.clone()
            });
        }
    });
    if let Some(key) = reshaped {
        // A reshaped key is checked like a captured one: it may now clash.
        match local_capture(keys, shortcut, Captured::Key(key)) {
            Some(Ok(key)) => {
                window.local_refusal =
                    (!overrides.set(project, shortcut, Some(key))).then_some(LocalRefusal::Limit);
            }
            Some(Err(refusal)) => window.local_refusal = Some(refusal),
            None => {}
        }
    }
    match use_project {
        Some(true) => {
            overrides.clear(project, shortcut);
            window.local_refusal = None;
        }
        Some(false) => {
            window.local_refusal =
                (!overrides.set(project, shortcut, None)).then_some(LocalRefusal::Limit);
        }
        None => {}
    }
    let refusal = match &window.local_refusal {
        Some(LocalRefusal::Unsupported) => Some(l.text(Msg::UserShortcutKeyUnsupported).to_owned()),
        Some(LocalRefusal::Reserved(keys)) => Some(l.format(
            Msg::UserShortcutKeyReserved,
            &[("keys", Arg::Str(&l.keys(keys)))],
        )),
        Some(LocalRefusal::Taken(name)) => {
            Some(l.format(Msg::UserShortcutLocalTaken, &[("name", Arg::Str(name))]))
        }
        Some(LocalRefusal::Limit) => {
            Some(l.tr(Msg::UserShortcutReasonOverrideLimit.en()).to_owned())
        }
        None => None,
    };
    if let Some(refusal) = refusal {
        ui.label(RichText::new(refusal).color(ERROR));
    } else if let Some(name) = shadowed {
        ui.label(
            RichText::new(l.format(Msg::UserShortcutLocalShadowed, &[("name", Arg::Str(&name))]))
                .color(muted_color()),
        );
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
    let capturing_then = window.capturing_then;
    let window_capturing_local = window.capturing_local;
    let unsupported = window.unsupported;
    let Some(draft) = window.draft.as_mut() else {
        return;
    };

    ui.horizontal(|ui| {
        form_label(ui, l.text(Msg::UserShortcutNameLabel));
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

    // The key: what it is, Set key (capture), Clear key, and its chord's
    // second key.
    let mut start_capture = None;
    ui.horizontal_wrapped(|ui| {
        form_label(ui, l.text(Msg::UserShortcutKeyLabel));
        // The key as a key cap, like the keyboard reference draws it.
        match (&draft.key, capturing) {
            (_, true) => ui.label(
                RichText::new(l.text(if capturing_then {
                    Msg::UserShortcutThenCapturing
                } else {
                    Msg::UserShortcutKeyCapturing
                }))
                .color(accent_color()),
            ),
            (Some(key), false) => crate::menu::key_cap(ui, &l.keys(&key_text(key)), 0.0),
            (None, false) => {
                ui.label(RichText::new(l.text(Msg::UserShortcutKeyNone)).color(muted_color()))
            }
        }
        .on_hover_text(l.text(Msg::UserShortcutKeyHover));
        ui.add_space(4.0);
        if ui
            .add_enabled(
                edit.is_none() && !capturing && !window_capturing_local,
                small_button(l.text(Msg::UserShortcutSetKey)),
            )
            .hint(
                l.text(Msg::UserShortcutSetKeyHover),
                edit.unwrap_or(Msg::UserShortcutReasonCapturing.en()),
            )
            .clicked()
        {
            start_capture = Some(false);
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
                small_button(l.text(Msg::UserShortcutClearKey)),
            )
            .hint(l.text(Msg::UserShortcutClearKeyHover), clear.unwrap_or(""))
            .clicked()
        {
            draft.key = None;
        }
        // A chord: its second key, or remove it.
        let chord = draft.key.as_ref().is_some_and(|key| key.then.is_some());
        if chord {
            let remove =
                edit.or_else(|| capturing.then_some(Msg::UserShortcutReasonCapturing.en()));
            if ui
                .add_enabled(
                    remove.is_none(),
                    small_button(l.text(Msg::UserShortcutClearThen)),
                )
                .hint(
                    l.text(Msg::UserShortcutClearThenHover),
                    remove.unwrap_or(""),
                )
                .clicked()
                && let Some(key) = &mut draft.key
            {
                key.then = None;
            }
        } else {
            let add = edit
                .or_else(|| {
                    (capturing || window_capturing_local)
                        .then_some(Msg::UserShortcutReasonCapturing.en())
                })
                .or_else(|| {
                    draft
                        .key
                        .is_none()
                        .then_some(Msg::UserShortcutReasonNoFirstKey.en())
                });
            if ui
                .add_enabled(
                    add.is_none(),
                    small_button(l.text(Msg::UserShortcutSetThen)),
                )
                .hint(l.text(Msg::UserShortcutSetThenHover), add.unwrap_or(""))
                .clicked()
            {
                start_capture = Some(true);
            }
        }
    });
    // Where the key answers: anywhere, in one panel, in one panel's
    // pop-out window, or in one tool window.
    ui.horizontal_wrapped(|ui| {
        form_label(ui, l.text(Msg::UserShortcutScopeLabel));
        let scope_block = edit.or_else(|| {
            draft
                .key
                .is_none()
                .then_some(Msg::UserShortcutReasonNoFirstKey.en())
        });
        let current = draft
            .key
            .as_ref()
            .and_then(|key| key.scope.as_deref())
            .and_then(AnswerScope::of);
        let name = |scope: Option<AnswerScope>| {
            scope.map_or_else(
                || l.text(Msg::UserShortcutScopeAnywhere).to_owned(),
                |scope| scope.label(l),
            )
        };
        let mut chosen = current;
        ui.add_enabled_ui(scope_block.is_none(), |ui| {
            ComboBox::from_id_salt("studio-user-shortcut-scope")
                .selected_text(name(current))
                .width(220.0)
                .show_ui(ui, |ui| {
                    for scope in core::iter::once(None).chain(AnswerScope::all().map(Some)) {
                        let hover = AnswerScope::hover(scope, Msg::UserShortcutScopeHover);
                        ui.selectable_value(&mut chosen, scope, name(scope))
                            .on_hover_text(l.text(hover));
                    }
                })
                .response
                .hint(
                    l.text(Msg::UserShortcutScopeHover),
                    scope_block.unwrap_or(""),
                );
        });
        if chosen != current
            && let Some(key) = &mut draft.key
        {
            key.scope = chosen.map(AnswerScope::key);
        }
    });
    if let Some(key) = &draft.key {
        if let Some(keys) = reserved_by(key, state.console_keymap()) {
            ui.label(
                RichText::new(l.format(
                    Msg::UserShortcutKeyReserved,
                    &[("keys", Arg::Str(&l.keys(&keys)))],
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
        form_label(ui, l.text(Msg::UserShortcutOscLabel));
        let mut text = draft.osc.clone().unwrap_or_default();
        let response = ui
            .add_enabled(
                edit.is_none(),
                TextEdit::singleline(&mut text)
                    .char_limit(MAX_SHORTCUT_OSC_BYTES)
                    .hint_text(l.text(Msg::UserShortcutOscHint))
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
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        let save = save_block(state, shortcuts, shortcut, &spec, dirty);
        // Save is the dialog's confirm key: accent, like Add input's.
        if ui
            .add_enabled(
                save.is_none(),
                Button::new(
                    RichText::new(l.text(Msg::UserShortcutSave))
                        .size(12.0)
                        .strong()
                        .color(text_color()),
                )
                .fill(ACCENT_FILL)
                .stroke(egui::Stroke::new(1.0, accent_color()))
                .min_size(egui::vec2(110.0, 26.0)),
            )
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
                footer_key(l.text(Msg::UserShortcutRevert)),
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
                .and_then(|found| fire_block(state, found).or_else(|| value_block(found, None)))
        };
        if ui
            .add_enabled(fire.is_none(), footer_key(l.text(Msg::UserShortcutFire)))
            .hint(l.text(Msg::UserShortcutFireHover), fire.unwrap_or(""))
            .clicked()
        {
            intents.push(StudioIntent::Shortcut(ShortcutIntent::Fire {
                shortcut,
                targets: fm_protocol::WireTargetBindings::default(),
                value: None,
            }));
        }
        let remove = edit.or_else(|| (!saved).then_some(Msg::UserShortcutReasonNotSaved.en()));
        if ui
            .add_enabled(
                remove.is_none(),
                footer_key(l.text(Msg::UserShortcutRemove)),
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
    if let Some(then) = start_capture {
        window.capturing = true;
        window.capturing_then = then;
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
        form_label(ui, l.text(Msg::UserShortcutActionLabel));
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
                // Under the Runs picker, in the fields' column.
                ui.add_space(FORM_LABEL_WIDTH + FORM_LABEL_GAP);
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
                ui.add_space(FORM_LABEL_WIDTH + FORM_LABEL_GAP);
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
pub(crate) fn reference_rows(
    l: Locale,
    state: &StudioUiState,
    overrides: &ShortcutKeyOverrides,
) -> Vec<(String, String)> {
    let console = ConsoleKeys::of(state, overrides);
    console
        .shortcuts
        .iter()
        .filter_map(|shortcut| {
            let key = console.key(shortcut);
            (key.is_some() || shortcut.spec.osc.is_some()).then_some((shortcut, key))
        })
        .map(|(shortcut, key)| {
            let keys = key.map_or_else(
                || l.text(Msg::UserShortcutOscOnly).to_owned(),
                |(key, local)| key_words(l, key, local),
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

/// A key of the editor's Save row: Save's text size and height, so the
/// row reads as one set of keys.
fn footer_key(text: &str) -> Button<'static> {
    Button::new(RichText::new(text).size(12.0)).min_size(egui::vec2(0.0, 26.0))
}

#[cfg(test)]
mod tests;
