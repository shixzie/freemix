//! The console's key chords and its complete key map (RC-018).
//!
//! The window and output keys are Ctrl+Shift chords (Cmd+Shift on macOS)
//! matched exactly, so a chord never also fires a neighbour that differs
//! only by Shift or Alt. The unit tests list every key the console and its
//! keyboard-owning panels handle, in the order their handlers read them,
//! and prove that no binding is listed twice or shadowed by an earlier one
//! in any context, that every console key is in the keyboard reference,
//! and that no key acts while a text field has focus.

use egui::{Event, Key, Modifiers, Ui};

use crate::{
    RecordingAction, StreamGroupAction, StudioIntent, StudioShell, StudioUiState, recording_action,
    stream_group_action,
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
}

impl Chord {
    /// Every chord.
    pub(crate) const ALL: [Self; 5] = [
        Self::Scripts,
        Self::LiveLan,
        Self::Record,
        Self::StartStreams,
        Self::StopStreams,
    ];

    /// The letter held with Ctrl+Shift.
    pub(crate) const fn key(self) -> Key {
        match self {
            Self::Scripts => Key::S,
            Self::LiveLan => Key::N,
            Self::Record => Key::R,
            Self::StartStreams => Key::O,
            Self::StopStreams => Key::K,
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
        }
    }

    /// Whether the chord opens a window (else it drives an output).
    const fn opens_window(self) -> bool {
        matches!(self, Self::Scripts | Self::LiveLan)
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
    /// Ctrl+Shift+S and Ctrl+Shift+N open or close the Scripts and `LiveLAN`
    /// windows unless a text field or a dialog owns the keyboard.
    pub(crate) fn toggle_chord_windows(&mut self, ui: &mut Ui, state: &StudioUiState) {
        if ui.ctx().egui_wants_keyboard_input()
            || self.show_shortcuts
            || self.show_about
            || self.show_preferences
            || self.add_input.is_some()
            || self.remove_input.is_some()
        {
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

/// The output chords' intents: REC 1 start or stop (whichever its desired
/// state calls for) and start or stop every stream destination, each only
/// when the state permits it, as the matching keys do. A chord whose action
/// is unavailable is swallowed. Nothing acts while a text field has focus.
pub(crate) fn output_chord_intents(ui: &mut Ui, state: &StudioUiState) -> Vec<StudioIntent> {
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
            Chord::Scripts | Chord::LiveLan => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use egui::{Event, Key, Modifiers};

    use super::{
        Chord, LIVE_LAN_SHORTCUT, RECORD_SHORTCUT, SCRIPTS_SHORTCUT, START_STREAMS_SHORTCUT,
        STOP_STREAMS_SHORTCUT, chord,
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
        // The window and output chords are listed in their own groups.
        for chord in Chord::ALL {
            let entry = STUDIO_SHORTCUTS
                .iter()
                .find(|entry| entry.keys == chord.keys())
                .unwrap_or_else(|| panic!("{chord:?} is not in the reference"));
            let group = if chord.opens_window() {
                ShortcutGroup::Windows
            } else {
                ShortcutGroup::Output
            };
            assert_eq!(entry.group, group, "{chord:?}");
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
        let events = keys
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
}
