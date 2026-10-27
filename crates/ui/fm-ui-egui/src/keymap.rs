//! The console's key chords and its complete key map (RC-018).
//!
//! The window, output, and cue keys are Ctrl+Shift chords (Cmd+Shift on macOS)
//! matched exactly, so a chord never also fires a neighbour that differs
//! only by Shift or Alt. The unit tests list every key the console and its
//! keyboard-owning panels handle, in the order their handlers read them,
//! and prove that no binding is listed twice or shadowed by an earlier one
//! in any context, that every console key is in the keyboard reference,
//! and that no key acts while a text field has focus.

use egui::{Event, Key, Modifiers, Ui};

use crate::{
    CueIntent, CueKey, RecordingAction, ShortcutGroup, StreamGroupAction, StudioIntent,
    StudioShell, StudioUiState, recording_action, stream_group_action,
};

/// Key text of the Scripts window chord.
pub(crate) const SCRIPTS_SHORTCUT: &str = "Ctrl+Shift+S";
/// Key text of the `LiveLAN` window chord.
pub(crate) const LIVE_LAN_SHORTCUT: &str = "Ctrl+Shift+N";
/// Key text of the REC 1 start/stop chord.
pub(crate) const RECORD_SHORTCUT: &str = "Ctrl+Shift+R";
/// Key text of the start-all-streams chord.
pub(crate) const START_STREAMS_SHORTCUT: &str = "Ctrl+Shift+O";
/// Key text of the stop-all-streams chord.
pub(crate) const STOP_STREAMS_SHORTCUT: &str = "Ctrl+Shift+K";
/// Key text of the Cues panel's Arm GO chord.
pub(crate) const CUE_ARM_SHORTCUT: &str = "Ctrl+Shift+M";
/// Key text of the Cues panel's Disarm chord.
pub(crate) const CUE_DISARM_SHORTCUT: &str = "Ctrl+Shift+D";
/// Key text of the Cues panel's Stop chord.
pub(crate) const CUE_STOP_SHORTCUT: &str = "Ctrl+Shift+X";
/// Key text of the Cues panel's Reset chord.
pub(crate) const CUE_RESET_SHORTCUT: &str = "Ctrl+Shift+E";

/// A Ctrl+Shift chord handled by the console.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Chord {
    /// Opens or closes the Scripts window.
    Scripts,
    /// Opens or closes the `LiveLAN` window.
    LiveLan,
    /// Starts or stops recorder REC 1.
    Record,
    /// Starts every stopped stream destination.
    StartStreams,
    /// Stops every running stream destination.
    StopStreams,
    /// Arms a GO of the Cues panel's standby cue (RC-017).
    CueArm,
    /// Drops the Cues panel's armed GO.
    CueDisarm,
    /// Stops the Cues panel's list.
    CueStop,
    /// Resets the Cues panel's list.
    CueReset,
}

impl Chord {
    /// Every chord.
    pub(crate) const ALL: [Self; 9] = [
        Self::Scripts,
        Self::LiveLan,
        Self::Record,
        Self::StartStreams,
        Self::StopStreams,
        Self::CueArm,
        Self::CueDisarm,
        Self::CueStop,
        Self::CueReset,
    ];

    /// The letter held with Ctrl+Shift.
    pub(crate) const fn key(self) -> Key {
        match self {
            Self::Scripts => Key::S,
            Self::LiveLan => Key::N,
            Self::Record => Key::R,
            Self::StartStreams => Key::O,
            Self::StopStreams => Key::K,
            Self::CueArm => Key::M,
            Self::CueDisarm => Key::D,
            Self::CueStop => Key::X,
            Self::CueReset => Key::E,
        }
    }

    /// The chord's key text, as the reference and hover help show it.
    pub(crate) const fn keys(self) -> &'static str {
        match self {
            Self::Scripts => SCRIPTS_SHORTCUT,
            Self::LiveLan => LIVE_LAN_SHORTCUT,
            Self::Record => RECORD_SHORTCUT,
            Self::StartStreams => START_STREAMS_SHORTCUT,
            Self::StopStreams => STOP_STREAMS_SHORTCUT,
            Self::CueArm => CUE_ARM_SHORTCUT,
            Self::CueDisarm => CUE_DISARM_SHORTCUT,
            Self::CueStop => CUE_STOP_SHORTCUT,
            Self::CueReset => CUE_RESET_SHORTCUT,
        }
    }

    /// The keyboard reference group that lists the chord.
    pub(crate) const fn group(self) -> ShortcutGroup {
        match self {
            Self::Scripts | Self::LiveLan => ShortcutGroup::Windows,
            Self::Record | Self::StartStreams | Self::StopStreams => ShortcutGroup::Output,
            Self::CueArm | Self::CueDisarm | Self::CueStop | Self::CueReset => ShortcutGroup::Cues,
        }
    }

    /// Whether the chord opens a window (else it drives an output or the
    /// Cues panel's list).
    const fn opens_window(self) -> bool {
        matches!(self.group(), ShortcutGroup::Windows)
    }
}

/// The chord a key press names: Ctrl+Shift (Cmd+Shift on macOS) and one of
/// the chord letters, without Alt.
pub(crate) fn chord(key: Key, modifiers: Modifiers) -> Option<Chord> {
    if !(modifiers.command && modifiers.shift) || modifiers.alt {
        return None;
    }
    Chord::ALL.into_iter().find(|chord| chord.key() == key)
}

/// The cue step a press of `key` names: `,` steps the Cues panel's standby
/// back and `.` forward (RC-017), whatever keys the layout types them with.
///
/// egui-winit reports a press as its logical key (the character the layout
/// produced, Shift applied), falling back to the physical key when that
/// character has no [`Key`]; the [`Event::Text`] it typed follows in the
/// same frame. So AZERTY's Shift+`;` arrives as [`Key::Period`] with Shift
/// and the text `.`, while US Shift+`.` (`>`, which has no [`Key`]) arrives
/// as the physical [`Key::Period`] with Shift and the text `>`. A press
/// counts when its `typed` text is the punctuation itself, or, without
/// typed text, when no modifier is held. Ctrl, Alt, and Cmd never count.
pub(crate) fn cue_step_key(key: Key, modifiers: Modifiers, typed: Option<&str>) -> Option<CueKey> {
    let (step, punctuation) = match key {
        Key::Comma => (CueKey::Previous, ","),
        Key::Period => (CueKey::Next, "."),
        _ => return None,
    };
    if modifiers.ctrl || modifiers.alt || modifiers.command || modifiers.mac_cmd {
        return None;
    }
    let produced = typed.map_or(!modifiers.shift, |typed| typed == punctuation);
    produced.then_some(step)
}

/// The cue step of each of `events` (see [`cue_step_key`]): `Some` only
/// for a press of `,` or `.`, read with the text the press typed.
pub(crate) fn cue_steps(events: &[Event]) -> Vec<Option<CueKey>> {
    events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                return None;
            };
            let typed = match events.get(index + 1) {
                Some(Event::Text(text)) => Some(text.as_str()),
                _ => None,
            };
            cue_step_key(*key, *modifiers, typed)
        })
        .collect()
}

/// Removes this frame's presses of the chords `wanted` accepts from the
/// input, so nothing else reads them, and returns the new (non-repeat)
/// presses in order.
fn take_chords(ui: &mut Ui, wanted: impl Fn(Chord) -> bool) -> Vec<Chord> {
    ui.input_mut(|input| {
        let mut taken = Vec::new();
        input.events.retain(|event| {
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
            let Some(chord) = chord(*key, *modifiers).filter(|chord| wanted(*chord)) else {
                return true;
            };
            if !repeat {
                taken.push(chord);
            }
            false
        });
        taken
    })
}

impl StudioShell {
    /// Whether a modal dialog other than the keyboard reference and
    /// Preferences (whose own keys close them) is open: About, Add or
    /// Remove input, an input-lifecycle dialog, the layout-name dialog, the
    /// Shortcuts window (whose "Set key" takes the next key press), or the
    /// Key profiles window (whose "Set key" does too).
    /// No console key or chord acts behind one.
    pub(crate) const fn modal_dialog_open(&self) -> bool {
        self.show_about
            || self.add_input.is_some()
            || self.remove_input.is_some()
            || self.lifecycle.dialog_open()
            || self.layout_dialog_open()
            || self.user_shortcuts.open
            || self.key_profiles_window.open
    }

    /// Whether any dialog blocks the window keys (Ctrl+G, Ctrl+Shift+A/L/S/N):
    /// the same modal gate the console keys and cue chords obey.
    pub(crate) const fn window_keys_blocked(&self) -> bool {
        self.show_shortcuts || self.show_preferences || self.modal_dialog_open()
    }

    /// Ctrl+Shift+S and Ctrl+Shift+N open or close the Scripts and `LiveLAN`
    /// windows unless a text field or a dialog owns the keyboard.
    pub(crate) fn toggle_chord_windows(&mut self, ui: &mut Ui, state: &StudioUiState) {
        if ui.ctx().egui_wants_keyboard_input() || self.window_keys_blocked() {
            return;
        }
        for chord in take_chords(ui, Chord::opens_window) {
            match chord {
                Chord::Scripts if self.scripts.open => self.scripts.open = false,
                Chord::Scripts => self.open_scripts(state),
                Chord::LiveLan => self.live_lan.open = !self.live_lan.open,
                _ => {}
            }
        }
    }
}

/// The output and cue chords' intents: REC 1 start or stop (whichever its
/// desired state calls for), start or stop every stream destination, and
/// the Cues panel's Arm GO, Disarm, Stop, and Reset (`cue_key` resolves
/// those with the panel keys' gates), each only when the state permits it,
/// as the matching keys do. A chord whose action is unavailable is
/// swallowed. Nothing acts while a text field has focus.
pub(crate) fn output_chord_intents(
    ui: &mut Ui,
    state: &StudioUiState,
    cue_key: &dyn Fn(CueKey) -> Option<CueIntent>,
) -> Vec<StudioIntent> {
    if ui.ctx().egui_wants_keyboard_input() {
        return Vec::new();
    }
    take_chords(ui, |chord| !chord.opens_window())
        .into_iter()
        .filter_map(|chord| match chord {
            Chord::Record => {
                let recorder = fm_protocol::WireRecorderSlot::new(1)?;
                let requested = if state.recording_desired[0] {
                    RecordingAction::Stop(recorder)
                } else {
                    RecordingAction::Start(recorder)
                };
                recording_action(state, requested).map(StudioIntent::Recording)
            }
            Chord::StartStreams => stream_group_action(state, StreamGroupAction::StartAll)
                .map(StudioIntent::StreamGroup),
            Chord::StopStreams => stream_group_action(state, StreamGroupAction::StopAll)
                .map(StudioIntent::StreamGroup),
            Chord::CueArm => cue_key(CueKey::Arm).map(StudioIntent::Cue),
            Chord::CueDisarm => cue_key(CueKey::Disarm).map(StudioIntent::Cue),
            Chord::CueStop => cue_key(CueKey::Stop).map(StudioIntent::Cue),
            Chord::CueReset => cue_key(CueKey::Reset).map(StudioIntent::Cue),
            Chord::Scripts | Chord::LiveLan => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use egui::{Event, Key, Modifiers};

    use super::{
        CUE_ARM_SHORTCUT, CUE_DISARM_SHORTCUT, CUE_RESET_SHORTCUT, CUE_STOP_SHORTCUT, Chord,
        LIVE_LAN_SHORTCUT, RECORD_SHORTCUT, SCRIPTS_SHORTCUT, START_STREAMS_SHORTCUT,
        STOP_STREAMS_SHORTCUT, chord, cue_step_key,
    };
    use crate::{
        RecordingAction, STUDIO_SHORTCUTS, ShortcutGroup, StreamGroupAction,
        StudioConnectionStatus, StudioIntent, StudioShell, StudioUiState,
    };

    /// When a binding's handler runs.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum KeyScope {
        /// Read first every frame, whichever panel owns the keyboard: help,
        /// preferences, and the window keys.
        Global,
        /// The console keys, read only while no dialog, menu, or panel owns
        /// the keyboard.
        Console,
        /// The Colour panel's keys while it owns the keyboard.
        ColorPanel,
        /// The animation timeline's keys while it owns the keyboard.
        AnimationTimeline,
    }

    /// How a handler compares the pressed modifiers with its own.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Matching {
        /// Exactly these modifiers.
        Exact,
        /// egui's logical match: extra Shift and Alt are ignored.
        Logical,
    }

    /// One key the console or a keyboard-owning panel handles.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct Binding {
        scope: KeyScope,
        modifiers: Modifiers,
        key: Key,
        matching: Matching,
        /// The key text of the reference entry that documents it.
        keys: &'static str,
    }

    impl Binding {
        /// Whether a press of `key` with `pressed` modifiers reaches this
        /// binding's handler.
        fn fires(self, key: Key, pressed: Modifiers) -> bool {
            key == self.key
                && match self.matching {
                    Matching::Exact => pressed.matches_exact(self.modifiers),
                    Matching::Logical => pressed.matches_logically(self.modifiers),
                }
        }
    }

    const fn binding(
        scope: KeyScope,
        modifiers: Modifiers,
        key: Key,
        matching: Matching,
        keys: &'static str,
    ) -> Binding {
        Binding {
            scope,
            modifiers,
            key,
            matching,
            keys,
        }
    }

    const NONE: Modifiers = Modifiers::NONE;
    const CMD: Modifiers = Modifiers::COMMAND;
    const CMD_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);

    use KeyScope::{AnimationTimeline, ColorPanel, Console, Global};
    use Matching::{Exact, Logical};

    /// Every key binding, per scope in the order the handlers read them:
    /// [`crate::preferences::preference_key`], [`crate::help_key`], the window
    /// toggles, [`crate::studio_shortcuts`], the edit history, the output
    /// chords, the Colour panel's `handle_keys`, and the animation timeline's
    /// transport keys. (Esc also restores a maximised tile when no manual
    /// transition takes it; that fallback is not a separate binding.)
    const BINDINGS: &[Binding] = &[
        binding(
            Global,
            CMD,
            Key::Comma,
            Exact,
            crate::preferences::PREFERENCES_SHORTCUT,
        ),
        binding(
            Global,
            CMD,
            Key::Equals,
            Exact,
            crate::preferences::UI_SCALE_SHORTCUT,
        ),
        binding(
            Global,
            CMD,
            Key::Plus,
            Exact,
            crate::preferences::UI_SCALE_SHORTCUT,
        ),
        binding(
            Global,
            CMD,
            Key::Minus,
            Exact,
            crate::preferences::UI_SCALE_SHORTCUT,
        ),
        binding(
            Global,
            CMD,
            Key::Num0,
            Exact,
            crate::preferences::UI_SCALE_RESET_SHORTCUT,
        ),
        binding(Global, NONE, Key::Questionmark, Logical, "? · F1"),
        binding(Global, NONE, Key::F1, Exact, "? · F1"),
        binding(
            Global,
            CMD,
            Key::G,
            Logical,
            crate::studio_color::COLOR_SHORTCUT,
        ),
        binding(
            Global,
            CMD_SHIFT,
            Key::A,
            Logical,
            crate::studio_animation::ANIMATION_SHORTCUT,
        ),
        binding(
            Global,
            CMD_SHIFT,
            Key::L,
            Logical,
            crate::scene_designer::SCENE_DESIGNER_SHORTCUT,
        ),
        binding(Global, CMD_SHIFT, Key::S, Exact, SCRIPTS_SHORTCUT),
        binding(Global, CMD_SHIFT, Key::N, Exact, LIVE_LAN_SHORTCUT),
        binding(Console, NONE, Key::B, Exact, "B"),
        binding(Console, NONE, Key::T, Exact, "T"),
        binding(Console, NONE, Key::PageUp, Exact, "PgUp / PgDn"),
        binding(Console, NONE, Key::PageDown, Exact, "PgUp / PgDn"),
        binding(Console, NONE, Key::End, Exact, "End"),
        binding(Console, NONE, Key::Escape, Exact, "Esc"),
        binding(Console, NONE, Key::C, Exact, "C"),
        binding(Console, NONE, Key::G, Exact, "G"),
        binding(Console, NONE, Key::Comma, Exact, ","),
        binding(Console, NONE, Key::Period, Exact, "."),
        binding(Console, NONE, Key::F, Exact, "F"),
        binding(Console, NONE, Key::F5, Exact, "F5 – F8"),
        binding(Console, NONE, Key::F6, Exact, "F5 – F8"),
        binding(Console, NONE, Key::F7, Exact, "F5 – F8"),
        binding(Console, NONE, Key::F8, Exact, "F5 – F8"),
        binding(Console, NONE, Key::Num1, Exact, "1 – 8"),
        binding(Console, NONE, Key::Num2, Exact, "1 – 8"),
        binding(Console, NONE, Key::Num3, Exact, "1 – 8"),
        binding(Console, NONE, Key::Num4, Exact, "1 – 8"),
        binding(Console, NONE, Key::Num5, Exact, "1 – 8"),
        binding(Console, NONE, Key::Num6, Exact, "1 – 8"),
        binding(Console, NONE, Key::Num7, Exact, "1 – 8"),
        binding(Console, NONE, Key::Num8, Exact, "1 – 8"),
        binding(Console, CMD, Key::Z, Exact, crate::UNDO_SHORTCUT),
        binding(Console, CMD_SHIFT, Key::Z, Exact, crate::REDO_SHORTCUT),
        binding(Console, CMD_SHIFT, Key::R, Exact, RECORD_SHORTCUT),
        binding(Console, CMD_SHIFT, Key::O, Exact, START_STREAMS_SHORTCUT),
        binding(Console, CMD_SHIFT, Key::K, Exact, STOP_STREAMS_SHORTCUT),
        binding(Console, CMD_SHIFT, Key::M, Exact, CUE_ARM_SHORTCUT),
        binding(Console, CMD_SHIFT, Key::D, Exact, CUE_DISARM_SHORTCUT),
        binding(Console, CMD_SHIFT, Key::X, Exact, CUE_STOP_SHORTCUT),
        binding(Console, CMD_SHIFT, Key::E, Exact, CUE_RESET_SHORTCUT),
        binding(ColorPanel, CMD_SHIFT, Key::Z, Logical, "Ctrl+Shift+Z"),
        binding(ColorPanel, CMD, Key::Y, Logical, "Ctrl+Y"),
        binding(ColorPanel, CMD, Key::Z, Logical, "Ctrl+Z"),
        binding(ColorPanel, CMD, Key::C, Logical, "Ctrl+C"),
        binding(ColorPanel, CMD, Key::V, Logical, "Ctrl+V"),
        binding(ColorPanel, NONE, Key::M, Logical, "M"),
        binding(ColorPanel, NONE, Key::B, Logical, "B"),
        binding(ColorPanel, NONE, Key::A, Logical, "A"),
        binding(ColorPanel, NONE, Key::W, Logical, "W"),
        binding(ColorPanel, NONE, Key::Escape, Logical, "Esc"),
        binding(
            AnimationTimeline,
            CMD_SHIFT,
            Key::Z,
            Logical,
            "Ctrl+Shift+Z",
        ),
        binding(AnimationTimeline, CMD, Key::Y, Logical, "Ctrl+Y"),
        binding(AnimationTimeline, CMD, Key::Z, Logical, "Ctrl+Z"),
        binding(AnimationTimeline, CMD, Key::ArrowLeft, Logical, "Ctrl+Left"),
        binding(
            AnimationTimeline,
            CMD,
            Key::ArrowRight,
            Logical,
            "Ctrl+Right",
        ),
        binding(AnimationTimeline, CMD, Key::A, Logical, "Ctrl+A"),
        binding(AnimationTimeline, CMD, Key::C, Logical, "Ctrl+C"),
        binding(AnimationTimeline, CMD, Key::V, Logical, "Ctrl+V"),
        binding(AnimationTimeline, CMD, Key::X, Logical, "Ctrl+X"),
        binding(AnimationTimeline, NONE, Key::Space, Logical, "Space"),
        binding(AnimationTimeline, NONE, Key::K, Logical, "K"),
        binding(AnimationTimeline, NONE, Key::L, Logical, "L"),
        binding(AnimationTimeline, NONE, Key::M, Logical, "M"),
        binding(AnimationTimeline, NONE, Key::Delete, Logical, "Delete"),
        binding(
            AnimationTimeline,
            NONE,
            Key::Backspace,
            Logical,
            "Backspace",
        ),
        binding(AnimationTimeline, NONE, Key::PageUp, Logical, "PageUp"),
        binding(AnimationTimeline, NONE, Key::PageDown, Logical, "PageDown"),
        binding(AnimationTimeline, NONE, Key::ArrowLeft, Logical, "Left"),
        binding(AnimationTimeline, NONE, Key::ArrowRight, Logical, "Right"),
        binding(AnimationTimeline, NONE, Key::Home, Logical, "Home"),
        binding(AnimationTimeline, NONE, Key::End, Logical, "End"),
        binding(AnimationTimeline, NONE, Key::Escape, Logical, "Esc"),
    ];

    /// The scopes that read keys while `context` owns the keyboard, in order.
    const fn scopes(context: KeyScope) -> [KeyScope; 2] {
        [KeyScope::Global, context]
    }

    /// A press with Ctrl/Cmd, Shift, and Alt as given, as egui reports it
    /// on Linux and Windows.
    const fn press(command: bool, shift: bool, alt: bool) -> Modifiers {
        Modifiers {
            alt,
            ctrl: command,
            shift,
            mac_cmd: false,
            command,
        }
    }

    /// The press a binding is meant for: exactly its own modifiers.
    const fn own_press(binding: Binding) -> Modifiers {
        press(
            binding.modifiers.command,
            binding.modifiers.shift,
            binding.modifiers.alt,
        )
    }

    /// The bindings live while `context` owns the keyboard, in the order
    /// their handlers read a press.
    fn live(context: KeyScope) -> Vec<Binding> {
        scopes(context)
            .into_iter()
            .flat_map(|scope| {
                BINDINGS
                    .iter()
                    .filter(move |binding| binding.scope == scope)
            })
            .copied()
            .collect()
    }

    #[test]
    fn every_binding_is_unique_and_reachable_in_its_context() {
        for context in [
            KeyScope::Console,
            KeyScope::ColorPanel,
            KeyScope::AnimationTimeline,
        ] {
            let live = live(context);
            for (index, binding) in live.iter().enumerate() {
                // The first handler that takes this binding's own press is
                // its own: no earlier binding repeats or shadows it.
                let first = live
                    .iter()
                    .position(|other| other.fires(binding.key, own_press(*binding)));
                assert_eq!(
                    first,
                    Some(index),
                    "{context:?}: {binding:?} is taken by {:?}",
                    first.map(|first| live[first])
                );
            }
        }
    }

    #[test]
    fn every_console_binding_is_in_the_reference_and_every_chord_is_bound() {
        for binding in BINDINGS
            .iter()
            .filter(|binding| matches!(binding.scope, KeyScope::Global | KeyScope::Console))
        {
            assert!(
                STUDIO_SHORTCUTS
                    .iter()
                    .any(|entry| entry.keys == binding.keys),
                "{binding:?} is not in the keyboard reference"
            );
        }
        for entry in STUDIO_SHORTCUTS
            .iter()
            .filter(|entry| entry.keys.starts_with("Ctrl"))
        {
            assert!(
                BINDINGS.iter().any(|binding| binding.keys == entry.keys),
                "{} is listed but not bound",
                entry.keys
            );
        }
        // The window, output, and cue chords are listed in their own groups.
        for chord in Chord::ALL {
            let entry = STUDIO_SHORTCUTS
                .iter()
                .find(|entry| entry.keys == chord.keys())
                .unwrap_or_else(|| panic!("{chord:?} is not in the reference"));
            assert_eq!(entry.group, chord.group(), "{chord:?}");
            assert_eq!(
                chord.opens_window(),
                chord.group() == ShortcutGroup::Windows
            );
        }
    }

    #[test]
    fn chords_need_exactly_ctrl_and_shift() {
        for wanted in Chord::ALL {
            let key = wanted.key();
            assert_eq!(chord(key, press(true, true, false)), Some(wanted));
            assert_eq!(
                chord(key, Modifiers::COMMAND | Modifiers::SHIFT),
                Some(wanted)
            );
            assert_eq!(chord(key, press(true, false, false)), None);
            assert_eq!(chord(key, press(false, true, false)), None);
            assert_eq!(chord(key, press(true, true, true)), None);
            assert_eq!(chord(key, Modifiers::NONE), None);
            assert!(
                BINDINGS.iter().any(|binding| binding.key == key
                    && binding.keys == wanted.keys()
                    && binding.matching == Matching::Exact),
                "{wanted:?}"
            );
        }
        assert_eq!(chord(Key::Z, press(true, true, false)), None);
    }

    /// A ready console with REC 1 stopped and one stopped stream.
    fn output_state() -> StudioUiState {
        let roster = vec![fm_ui_model::StreamStatus {
            target: core::num::NonZeroU128::new(40).unwrap(),
            name: "RTMP Primary".to_owned(),
            desired_running: false,
            realized: fm_protocol::StreamRealizedState::Stopped,
            detail: None,
        }];
        let mut view = crate::tests::recording_view([false, false]);
        view.streams.clone_from(&roster);
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_view(view)
            .with_switcher_permissions(true, true);
        state.streams = roster;
        state
    }

    /// Runs one frame that presses each of `keys`, optionally with a text
    /// field focused, through the preference keys, the window toggles, and
    /// the console keys; returns the console's intents.
    fn run_keys(
        shell: &mut StudioShell,
        state: &StudioUiState,
        keys: &[(Key, Modifiers)],
        typing: bool,
    ) -> Vec<StudioIntent> {
        let context = egui::Context::default();
        let events: Vec<Event> = keys
            .iter()
            .flat_map(|&(key, modifiers)| {
                [true, false].map(|pressed| Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                })
            })
            .collect();
        run_frame(&context, shell, state, events, typing)
    }

    /// Runs one frame of `events` on `context`, as [`run_keys`] does.
    fn run_frame(
        context: &egui::Context,
        shell: &mut StudioShell,
        state: &StudioUiState,
        events: Vec<Event>,
        typing: bool,
    ) -> Vec<StudioIntent> {
        context.begin_pass(egui::RawInput {
            events,
            ..Default::default()
        });
        if typing {
            context.memory_mut(|memory| memory.request_focus(egui::Id::new("typing")));
        }
        let mut ui = egui::Ui::new(
            context.clone(),
            egui::Id::new("studio-keymap"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(context.viewport_rect()),
        );
        shell.handle_preference_keys(&mut ui);
        shell.toggle_windows_on_keys(&mut ui, state);
        let manual = crate::manual_transition_availability(
            crate::ManualTransitionGate::from_state(state),
            false,
        );
        let intents = shell.keyboard_intents(&mut ui, state, manual);
        drop(ui);
        let _ = context.end_pass();
        intents
    }

    fn chord_presses() -> Vec<(Key, Modifiers)> {
        Chord::ALL
            .map(|chord| (chord.key(), press(true, true, false)))
            .to_vec()
    }

    #[test]
    fn chords_open_windows_and_drive_outputs() {
        let mut shell = StudioShell::default();
        let intents = run_keys(&mut shell, &output_state(), &chord_presses(), false);
        let recorder = fm_protocol::WireRecorderSlot::new(1).unwrap();
        // Stop all has nothing to stop yet.
        assert_eq!(
            intents,
            vec![
                StudioIntent::Recording(RecordingAction::Start(recorder)),
                StudioIntent::StreamGroup(StreamGroupAction::StartAll),
            ]
        );
        assert!(shell.scripts.open);
        assert!(shell.live_lan.open);
        // The window chords close the windows again; with REC 1 recording
        // and the stream running, the output chords stop them.
        let mut running = output_state();
        running.recording_desired[0] = true;
        running.streams[0].desired_running = true;
        let intents = run_keys(&mut shell, &running, &chord_presses(), false);
        assert_eq!(
            intents,
            vec![
                StudioIntent::Recording(RecordingAction::Stop(recorder)),
                StudioIntent::StreamGroup(StreamGroupAction::StopAll),
            ]
        );
        assert!(!shell.scripts.open);
        assert!(!shell.live_lan.open);
    }

    /// RC-017: `,` and `.` step the Cues panel's standby and the cue chords
    /// arm, disarm, stop, and reset its list, as the panel's keys do.
    #[test]
    fn cue_keys_drive_the_cue_panels_list() {
        use fm_protocol::CueOperation;
        // List 1 has cue 20 on standby, armed.
        let state = crate::hover_audit::cue_ran_script_fixture();
        let mut shell = StudioShell::default();
        let presses = [
            (Key::Comma, Modifiers::NONE),
            (Key::Period, Modifiers::NONE),
            (Key::M, press(true, true, false)),
            (Key::D, press(true, true, false)),
            (Key::X, press(true, true, false)),
            (Key::E, press(true, true, false)),
        ];
        let operations: Vec<CueOperation> = run_keys(&mut shell, &state, &presses, false)
            .into_iter()
            .map(|intent| match intent {
                StudioIntent::Cue(crate::CueIntent::Control { list: 1, operation }) => operation,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            operations,
            vec![
                CueOperation::Back,
                CueOperation::Skip,
                CueOperation::Arm {
                    cue: None,
                    in_ms: 60_000,
                    targets: fm_protocol::WireTargetBindings::default()
                },
                CueOperation::Disarm,
                CueOperation::Stop,
                CueOperation::Reset,
            ]
        );
        assert_eq!(
            shell.cue_key_intent(&state, crate::CueKey::ArmIn(5_000)),
            Some(StudioIntent::Cue(crate::CueIntent::Control {
                list: 1,
                operation: CueOperation::Arm {
                    cue: None,
                    in_ms: 5_000,
                    targets: fm_protocol::WireTargetBindings::default()
                },
            }))
        );
        // RC-004: an external GO or arm carries the console's selection,
        // as the panel's does.
        let selected = fm_types::InputId::new(core::num::NonZeroU128::new(3).unwrap());
        shell.selected_input = Some(selected);
        assert_eq!(
            shell.cue_key_intent(&state, crate::CueKey::ArmIn(5_000)),
            Some(StudioIntent::Cue(crate::CueIntent::Control {
                list: 1,
                operation: CueOperation::Arm {
                    cue: None,
                    in_ms: 5_000,
                    targets: fm_protocol::WireTargetBindings {
                        selected: Some(fm_protocol::WireInputId::from_domain(selected)),
                        ..fm_protocol::WireTargetBindings::default()
                    }
                },
            }))
        );
        shell.selected_input = None;
        // Without the transition permission no cue key acts.
        let viewer = state.clone().with_switcher_permissions(true, false);
        assert_eq!(run_keys(&mut shell, &viewer, &presses, false), Vec::new());
        assert_eq!(shell.cue_key_intent(&viewer, crate::CueKey::Stop), None);
    }

    /// RC-017: holding G, `,`, `.`, or a cue chord acts once. The OS repeats
    /// the press without a release; egui marks those presses as repeats.
    #[test]
    fn a_held_cue_key_acts_once() {
        let state = crate::hover_audit::cue_ran_script_fixture();
        let shell = StudioShell::default();
        let context = egui::Context::default();
        let manual = crate::manual_transition_availability(
            crate::ManualTransitionGate::from_state(&state),
            false,
        );
        let mut sent = Vec::new();
        for (key, modifiers) in [
            (Key::G, Modifiers::NONE),
            (Key::Comma, Modifiers::NONE),
            (Key::Period, Modifiers::NONE),
            (Key::X, press(true, true, false)),
        ] {
            let mut operations = Vec::new();
            // Three frames of the key held down: one press, two OS repeats.
            for _ in 0..3 {
                context.begin_pass(egui::RawInput {
                    events: vec![Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers,
                    }],
                    ..Default::default()
                });
                let mut ui = egui::Ui::new(
                    context.clone(),
                    egui::Id::new("studio-keymap-held"),
                    egui::UiBuilder::new()
                        .layer_id(egui::LayerId::background())
                        .max_rect(context.viewport_rect()),
                );
                operations.extend(shell.keyboard_intents(&mut ui, &state, manual));
                drop(ui);
                let _ = context.end_pass();
            }
            // Released before the next key.
            context.begin_pass(egui::RawInput {
                events: vec![Event::Key {
                    key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            });
            let _ = context.end_pass();
            assert_eq!(operations.len(), 1, "{key:?}: {operations:?}");
            sent.extend(operations);
        }
        let operations: Vec<fm_protocol::CueOperation> = sent
            .into_iter()
            .map(|intent| match intent {
                StudioIntent::Cue(crate::CueIntent::Control { list: 1, operation }) => operation,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            operations,
            [
                fm_protocol::CueOperation::Go {
                    cue: None,
                    targets: fm_protocol::WireTargetBindings::default()
                },
                fm_protocol::CueOperation::Back,
                fm_protocol::CueOperation::Skip,
                fm_protocol::CueOperation::Stop,
            ]
        );
    }

    #[test]
    fn cue_steps_follow_the_typed_character() {
        use crate::CueKey::{Next, Previous};
        let shift = Modifiers::SHIFT;
        // US layout: bare `,` and `.`, with or without their text.
        assert_eq!(cue_step_key(Key::Comma, NONE, None), Some(Previous));
        assert_eq!(cue_step_key(Key::Period, NONE, None), Some(Next));
        assert_eq!(cue_step_key(Key::Comma, NONE, Some(",")), Some(Previous));
        assert_eq!(cue_step_key(Key::Period, NONE, Some(".")), Some(Next));
        // AZERTY: `.` is Shift+`;`; its logical key is Period, typing `.`.
        assert_eq!(cue_step_key(Key::Period, shift, Some(".")), Some(Next));
        // US Shift+`.` types `>` (no egui key, so the physical Period).
        assert_eq!(cue_step_key(Key::Period, shift, Some(">")), None);
        assert_eq!(cue_step_key(Key::Comma, shift, Some("<")), None);
        assert_eq!(cue_step_key(Key::Period, shift, None), None);
        // A layout whose key at the physical `.` types something else.
        assert_eq!(cue_step_key(Key::Period, NONE, Some(":")), None);
        // Ctrl, Alt, and Cmd never step.
        for held in [CMD, Modifiers::CTRL, Modifiers::ALT, Modifiers::MAC_CMD] {
            assert_eq!(cue_step_key(Key::Period, held, Some(".")), None);
            assert_eq!(cue_step_key(Key::Comma, held, None), None);
        }
        assert_eq!(cue_step_key(Key::G, NONE, Some(".")), None);
    }

    /// AZERTY's Shift+`;` types `.` and steps the standby on, once while
    /// held; US Shift+`.` (`>`) does not, nor does a press while a text
    /// field has focus.
    #[test]
    fn a_shifted_period_steps_the_cue_list_on_layouts_that_need_shift() {
        use fm_protocol::CueOperation;
        let state = crate::hover_audit::cue_ran_script_fixture();
        let mut shell = StudioShell::default();
        let press = |physical: Key, text: &str, pressed: bool| {
            let key = Event::Key {
                key: Key::Period,
                physical_key: Some(physical),
                pressed,
                repeat: false,
                modifiers: Modifiers::SHIFT,
            };
            if pressed {
                vec![key, Event::Text(text.to_owned())]
            } else {
                vec![key]
            }
        };
        let operations = |intents: Vec<StudioIntent>| -> Vec<CueOperation> {
            intents
                .into_iter()
                .map(|intent| match intent {
                    StudioIntent::Cue(crate::CueIntent::Control { list: 1, operation }) => {
                        operation
                    }
                    other => panic!("unexpected {other:?}"),
                })
                .collect()
        };
        let context = egui::Context::default();
        let mut sent = Vec::new();
        // Held for three frames: one press, two OS repeats, then released.
        for _ in 0..3 {
            sent.extend(run_frame(
                &context,
                &mut shell,
                &state,
                press(Key::Comma, ".", true),
                false,
            ));
        }
        sent.extend(run_frame(
            &context,
            &mut shell,
            &state,
            press(Key::Comma, ".", false),
            false,
        ));
        assert_eq!(operations(sent), [CueOperation::Skip]);
        // US Shift+`.` types `>`.
        let intents = run_frame(
            &context,
            &mut shell,
            &state,
            press(Key::Period, ">", true),
            false,
        );
        assert_eq!(intents, Vec::new());
        // A focused text field takes the `.`.
        let typing = egui::Context::default();
        let intents = run_frame(
            &typing,
            &mut shell,
            &state,
            press(Key::Comma, ".", true),
            true,
        );
        assert_eq!(intents, Vec::new());
    }

    /// The window chords obey the modal gate the cue and output chords do:
    /// nothing acts behind an input-lifecycle dialog or the layout-name
    /// dialog, even before its text field takes focus.
    #[test]
    fn no_key_acts_behind_a_modal_dialog() {
        type Open = fn(&mut StudioShell);
        let state = output_state();
        let presses: Vec<(Key, Modifiers)> = BINDINGS
            .iter()
            .filter(|binding| matches!(binding.scope, KeyScope::Global | KeyScope::Console))
            .map(|binding| (binding.key, own_press(*binding)))
            .collect();
        let dialogs: [(&str, Open); 2] = [
            ("lifecycle", |shell| {
                shell.open_replace_source(
                    fm_types::InputId::new(core::num::NonZeroU128::new(2).unwrap()),
                    "Camera 2",
                );
            }),
            ("layout name", |shell| shell.open_layout_name_dialog(None)),
        ];
        for (dialog, open) in dialogs {
            let mut shell = StudioShell::default();
            open(&mut shell);
            assert!(shell.modal_dialog_open(), "{dialog}");
            let intents = run_keys(&mut shell, &state, &presses, false);
            assert_eq!(intents, Vec::new(), "{dialog}");
            assert!(!shell.show_shortcuts, "{dialog}");
            assert!(!shell.show_preferences, "{dialog}");
            assert!(!shell.scripts.open, "{dialog}");
            assert!(!shell.live_lan.open, "{dialog}");
            assert!(!shell.color.open, "{dialog}");
            assert!(!shell.animation.open, "{dialog}");
            assert!(!shell.designer.open, "{dialog}");
        }
    }

    #[test]
    fn no_shortcut_acts_while_a_text_field_has_focus() {
        let state = output_state();
        let presses: Vec<(Key, Modifiers)> = BINDINGS
            .iter()
            .filter(|binding| matches!(binding.scope, KeyScope::Global | KeyScope::Console))
            .map(|binding| (binding.key, own_press(*binding)))
            .collect();
        let mut shell = StudioShell::default();
        let intents = run_keys(&mut shell, &state, &presses, true);
        assert_eq!(intents, Vec::new());
        assert!(!shell.show_shortcuts);
        assert!(!shell.show_preferences);
        assert!(!shell.scripts.open);
        assert!(!shell.live_lan.open);
        assert!(!shell.color.open);
        assert!(!shell.animation.open);
        assert!(!shell.designer.open);
        // The same presses act once nothing holds the keyboard.
        let intents = run_keys(&mut shell, &state, &chord_presses(), false);
        assert_eq!(intents.len(), 2);
        assert!(shell.scripts.open && shell.live_lan.open);
    }

    /// RC-004 key profiles: the vMix style profile answers its keys (Space
    /// cuts, Enter fades, Ctrl+1 cuts input 1 straight to Program, Alt+1
    /// fades it there, Ctrl+B fades to black, Ctrl+F1 takes favourite 1),
    /// and `FreeMix`'s own `C`, `F`, and `B` do nothing; a project shortcut
    /// may then take `C` but not Space.
    #[test]
    fn the_vmix_profile_drives_the_console_keys() {
        let mut state = output_state();
        let mut shell = StudioShell::default();
        let mut profiles = shell.console_key_profiles().clone();
        assert!(profiles.choose(crate::ProfileChoice::Builtin(crate::BuiltinProfile::Vmix)));
        shell.set_console_key_profiles(profiles);
        shell.sync_console_keymap(&mut state);
        assert_eq!(
            state.console_keymap(),
            &crate::BuiltinProfile::Vmix.keymap()
        );
        let first = state.view.as_ref().unwrap().inputs[0];
        let duration_frames = shell.transition_duration_frames;
        let intents = run_keys(
            &mut shell,
            &state,
            &[
                (Key::Space, NONE),
                (Key::Enter, NONE),
                (Key::Num1, press(true, false, false)),
                (Key::Num1, press(false, false, true)),
                (Key::Num1, NONE),
                (Key::C, NONE),
                (Key::F, NONE),
                (Key::B, NONE),
            ],
            false,
        );
        assert_eq!(
            intents,
            vec![
                StudioIntent::Cut,
                StudioIntent::Fade { duration_frames },
                StudioIntent::SelectPreview(first),
                StudioIntent::Cut,
                StudioIntent::SelectPreview(first),
                StudioIntent::Fade { duration_frames },
                StudioIntent::SelectPreview(first),
            ]
        );
        let intents = run_keys(
            &mut shell,
            &state,
            &[
                (Key::B, press(true, false, false)),
                (Key::F1, press(true, false, false)),
            ],
            false,
        );
        assert!(
            matches!(&intents[..], [StudioIntent::ToggleFadeToBlack { .. }, _]),
            "{intents:?}"
        );
        // The profile's keys, not FreeMix's, are reserved from shortcuts.
        let key = |name: &str| fm_protocol::WireShortcutKey::stroke(name, false, false, false);
        assert_eq!(
            crate::user_shortcuts::reserved_by(&key("C"), state.console_keymap()),
            None
        );
        assert_eq!(
            crate::user_shortcuts::reserved_by(&key("Space"), state.console_keymap()).as_deref(),
            Some("Space")
        );
        assert_eq!(
            crate::user_shortcuts::reserved_by(
                &fm_protocol::WireShortcutKey::stroke("3", true, false, false),
                state.console_keymap()
            )
            .as_deref(),
            Some("Ctrl+3")
        );
        // Back on FreeMix the state forgets the profile.
        let mut profiles = shell.console_key_profiles().clone();
        assert!(profiles.choose(crate::ProfileChoice::default()));
        shell.set_console_key_profiles(profiles);
        shell.sync_console_keymap(&mut state);
        assert_eq!(state.console_keys, None);
    }

    /// RC-004: the keys no user shortcut may take mirror, in order, every
    /// key the console reads in every context and while no panel owns the
    /// keyboard outside the key profiles, so a new console key is reserved
    /// too; the `FreeMix` profile's keys are exactly the rest, which
    /// [`crate::user_shortcuts::reserved_by`] reserves through the active
    /// profile.
    #[test]
    fn user_shortcuts_reserve_every_console_key() {
        let profile = crate::console_keymap::freemix();
        let in_profile = |binding: &&Binding| {
            binding.scope == Console && profile.action(binding.key, own_press(**binding)).is_some()
        };
        for (_, stroke) in profile.bindings() {
            assert!(
                BINDINGS.iter().filter(in_profile).any(|binding| {
                    binding.key == stroke.key && binding.modifiers == stroke.modifiers()
                }),
                "{} is not a console binding",
                stroke.text()
            );
        }
        let mirrored =
            &crate::user_shortcuts::RESERVED_KEYS[..crate::user_shortcuts::MIRRORED_KEYS];
        let reserved: Vec<_> = mirrored
            .iter()
            .map(|key| (key.key, key.modifiers, key.logical, key.keys))
            .collect();
        let console: Vec<_> = BINDINGS
            .iter()
            .filter(|binding| matches!(binding.scope, Global | Console))
            .filter(|binding| !in_profile(binding))
            .map(|binding| {
                (
                    binding.key,
                    binding.modifiers,
                    binding.matching == Logical,
                    binding.keys,
                )
            })
            .collect();
        assert_eq!(reserved, console);
    }
}
