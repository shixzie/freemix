//! Console key profiles (RC-004): which keys drive the console's switching,
//! transition, overlay, replay, and cue actions on this console.
//!
//! The console's own keys come in profiles. Two are built in: **`FreeMix`**
//! (the keys the console always had: `1`–`8` to Preview, `C` Cut, `F` Fade,
//! `B` Fade to Black, `F5`–`F8` the favourite transitions, `G` the cue GO)
//! and **vMix style**, which follows the layout vMix operators commonly give
//! vMix's shortcut functions: `1`–`9` `PreviewInput`, `Ctrl+1`–`9`
//! `ActiveInput` (straight to Program on a cut), `Alt+1`–`9` `QuickPlay`
//! (straight to Program on the Fade), `Space` `Cut`, `Enter` `Fade`,
//! `Ctrl+B` `FadeToBlack`, `Ctrl+F1`–`F4` `Transition1`–`4`, `F9`–`F12`
//! `OverlayInput1`–`4` (the Preview input on the overlay, or off while
//! on), and `I`, `O`, and `E` `ReplayMarkIn`, `ReplayMarkOut`, and the
//! capture of the marked event. The `FreeMix` profile leaves the overlay
//! and replay actions without a key. (vMix
//! itself ships no fixed key map; its keys are whatever the operator binds
//! in its Shortcuts settings.) An operator may make up to
//! [`MAX_CUSTOM_PROFILES`] profiles of their own, each a copy of a built-in
//! one with any action given another key or none, and choose which profile
//! the console answers.
//!
//! A profile names at most one key per action and never one key for two
//! actions. The keys outside every profile stay fixed: help, preferences,
//! the window and output chords, undo and redo, the T-bar keys, the cue
//! step keys `,` and `.`, and Tab; a profile cannot take them. `Space` and
//! `Enter` press the focused control while one has keyboard focus, and
//! reach a profile's action only while none does. A project shortcut's key
//! never shadows a key the active profile uses (see
//! [`crate::user_shortcuts`]).
//!
//! Profiles are console preferences, saved with the others and never sent
//! to the daemon:
//!
//! ```text
//! keymap.profile = freemix | vmix | custom.<n>
//! keymap.custom.<n>.name = Late show
//! keymap.custom.<n>.base = vmix
//! keymap.custom.<n>.<action> = Ctrl+Shift+F9 | none
//! ```
//!
//! A profile also travels as a `.fmkeys` file of the same lines without
//! the `keymap.custom.<n>.` prefix (`name = …`, `base = …`, one line per
//! changed action), so an operator carries their keys to another console.
//!
//! where `<n>` is the custom profile's position from 1 and `<action>` an
//! [`ConsoleAction::token`] (`preview.3`, `take.3`, `quick_play.3`, `cut`,
//! `fade`, `fade_to_black`, `favorite.2`, `overlay.1`, `cue_go`,
//! `replay_mark_in`, `replay_mark_out`, `replay_capture`); only the actions
//! whose key differs from the base profile are written.

use std::collections::BTreeMap;

use egui::{Key, Modifiers};

use crate::l10n::{Arg, Locale, Msg};

/// Most profiles of their own an operator keeps.
pub const MAX_CUSTOM_PROFILES: usize = 8;
/// Longest custom profile name, in UTF-8 bytes.
pub const MAX_PROFILE_NAME_BYTES: usize = 48;
/// Inputs the numbered actions reach (`Preview 1` through `Preview 9`).
pub const NUMBERED_INPUTS: u8 = 9;
/// Favourite transition buttons a key may take.
pub const FAVORITE_KEYS: u8 = 4;
/// Overlay channels a key may turn on or off.
pub const OVERLAY_KEYS: u8 = 4;

const PREFIX: &str = "keymap.";
const PROFILE_KEY: &str = "keymap.profile";
const CUSTOM_PREFIX: &str = "keymap.custom.";
const NONE: &str = "none";

/// One console action a profile gives a key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ConsoleAction {
    /// The `n`th input (1 to [`NUMBERED_INPUTS`]) to Preview.
    Preview(u8),
    /// The `n`th input straight to Program on a cut: Preview, then Cut
    /// (vMix's `ActiveInput`).
    Take(u8),
    /// The `n`th input straight to Program on the Fade: Preview, then Fade
    /// (vMix's `QuickPlay`).
    QuickPlay(u8),
    Cut,
    /// The automatic Fade at the console's transition length.
    Fade,
    FadeToBlack,
    /// Favourite transition button `n` (1 to [`FAVORITE_KEYS`]).
    Favorite(u8),
    /// Overlay channel `n` (1 to [`OVERLAY_KEYS`]): the Preview input on
    /// it, or off while it is on (vMix's `OverlayInputN`).
    Overlay(u8),
    /// GO on the Cues panel's list.
    CueGo,
    /// Step the slideshow on Program (else Preview) to its next slide.
    SlideNext,
    /// Step the slideshow on Program (else Preview) to its previous slide.
    SlidePrevious,
    /// The replay in mark at the live frame.
    ReplayMarkIn,
    /// The replay out mark at the live frame.
    ReplayMarkOut,
    /// Capture the marked range as a replay event.
    ReplayCapture,
    /// Play the console's Replay player, or pause it while it plays.
    ReplayPlayPause,
    /// Step the Replay player one frame back, paused.
    ReplayStepBack,
    /// Step the Replay player one frame on, paused.
    ReplayStepForward,
    /// Shuttle the Replay player backwards at 2x.
    ReplayShuttleBack,
    /// Shuttle the Replay player forwards at 2x.
    ReplayShuttleForward,
    /// Play the second Replay input's player, or pause it (OR-012): two
    /// controllers can drive A and B independently.
    ReplayPlayPauseB,
    /// Step the second Replay input's player one frame back, paused.
    ReplayStepBackB,
    /// Step the second Replay input's player one frame on, paused.
    ReplayStepForwardB,
    /// Shuttle the second Replay input's player backwards at 2x.
    ReplayShuttleBackB,
    /// Shuttle the second Replay input's player forwards at 2x.
    ReplayShuttleForwardB,
}

impl ConsoleAction {
    /// Every action, in the order the profile editor lists them.
    #[must_use]
    pub fn all() -> Vec<Self> {
        let numbered = |make: fn(u8) -> Self| (1..=NUMBERED_INPUTS).map(make);
        numbered(Self::Preview)
            .chain(numbered(Self::Take))
            .chain(numbered(Self::QuickPlay))
            .chain([Self::Cut, Self::Fade, Self::FadeToBlack])
            .chain((1..=FAVORITE_KEYS).map(Self::Favorite))
            .chain((1..=OVERLAY_KEYS).map(Self::Overlay))
            .chain([
                Self::CueGo,
                Self::SlideNext,
                Self::SlidePrevious,
                Self::ReplayMarkIn,
                Self::ReplayMarkOut,
                Self::ReplayCapture,
                Self::ReplayPlayPause,
                Self::ReplayStepBack,
                Self::ReplayStepForward,
                Self::ReplayShuttleBack,
                Self::ReplayShuttleForward,
                Self::ReplayPlayPauseB,
                Self::ReplayStepBackB,
                Self::ReplayStepForwardB,
                Self::ReplayShuttleBackB,
                Self::ReplayShuttleForwardB,
            ])
            .collect()
    }

    /// The action's name in the preferences file.
    #[must_use]
    pub fn token(self) -> String {
        match self {
            Self::Preview(n) => format!("preview.{n}"),
            Self::Take(n) => format!("take.{n}"),
            Self::QuickPlay(n) => format!("quick_play.{n}"),
            Self::Cut => "cut".to_owned(),
            Self::Fade => "fade".to_owned(),
            Self::FadeToBlack => "fade_to_black".to_owned(),
            Self::Favorite(n) => format!("favorite.{n}"),
            Self::Overlay(n) => format!("overlay.{n}"),
            Self::CueGo => "cue_go".to_owned(),
            Self::SlideNext => "slide_next".to_owned(),
            Self::SlidePrevious => "slide_previous".to_owned(),
            Self::ReplayMarkIn => "replay_mark_in".to_owned(),
            Self::ReplayMarkOut => "replay_mark_out".to_owned(),
            Self::ReplayCapture => "replay_capture".to_owned(),
            Self::ReplayPlayPause => "replay_play_pause".to_owned(),
            Self::ReplayStepBack => "replay_step_back".to_owned(),
            Self::ReplayStepForward => "replay_step_forward".to_owned(),
            Self::ReplayShuttleBack => "replay_shuttle_back".to_owned(),
            Self::ReplayShuttleForward => "replay_shuttle_forward".to_owned(),
            Self::ReplayPlayPauseB => "replay_play_pause_b".to_owned(),
            Self::ReplayStepBackB => "replay_step_back_b".to_owned(),
            Self::ReplayStepForwardB => "replay_step_forward_b".to_owned(),
            Self::ReplayShuttleBackB => "replay_shuttle_back_b".to_owned(),
            Self::ReplayShuttleForwardB => "replay_shuttle_forward_b".to_owned(),
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::all()
            .into_iter()
            .find(|action| action.token() == token)
    }

    /// The keyboard reference group that lists the action.
    #[must_use]
    pub(crate) const fn group(self) -> crate::ShortcutGroup {
        match self {
            Self::Preview(_)
            | Self::Take(_)
            | Self::QuickPlay(_)
            | Self::Cut
            | Self::Overlay(_)
            | Self::SlideNext
            | Self::SlidePrevious => crate::ShortcutGroup::Switching,
            Self::Fade | Self::FadeToBlack | Self::Favorite(_) => crate::ShortcutGroup::Transitions,
            Self::CueGo => crate::ShortcutGroup::Cues,
            Self::ReplayMarkIn
            | Self::ReplayMarkOut
            | Self::ReplayCapture
            | Self::ReplayPlayPause
            | Self::ReplayStepBack
            | Self::ReplayStepForward
            | Self::ReplayShuttleBack
            | Self::ReplayShuttleForward
            | Self::ReplayPlayPauseB
            | Self::ReplayStepBackB
            | Self::ReplayStepForwardB
            | Self::ReplayShuttleBackB
            | Self::ReplayShuttleForwardB => crate::ShortcutGroup::Output,
        }
    }

    /// The action as the profile editor and the keyboard reference name it.
    #[must_use]
    pub fn label(self, l: Locale) -> String {
        let numbered = |msg, n: u8| l.format(msg, &[("n", Arg::Count(u64::from(n)))]);
        match self {
            Self::Preview(n) => numbered(Msg::ConsoleActionPreview, n),
            Self::Take(n) => numbered(Msg::ConsoleActionTake, n),
            Self::QuickPlay(n) => numbered(Msg::ConsoleActionQuickPlay, n),
            Self::Cut => l.text(Msg::ShortcutCut).to_owned(),
            Self::Fade => l.text(Msg::ShortcutFade).to_owned(),
            Self::FadeToBlack => l.text(Msg::ShortcutFadeToBlack).to_owned(),
            Self::Favorite(n) => numbered(Msg::ConsoleActionFavorite, n),
            Self::Overlay(n) => numbered(Msg::ConsoleActionOverlay, n),
            Self::CueGo => l.text(Msg::ShortcutCueGo).to_owned(),
            Self::SlideNext => l.text(Msg::CueActSlideNext).to_owned(),
            Self::SlidePrevious => l.text(Msg::CueActSlidePrevious).to_owned(),
            Self::ReplayMarkIn => l.text(Msg::ConsoleActionReplayMarkIn).to_owned(),
            Self::ReplayMarkOut => l.text(Msg::ConsoleActionReplayMarkOut).to_owned(),
            Self::ReplayCapture => l.text(Msg::ConsoleActionReplayCapture).to_owned(),
            Self::ReplayPlayPause => l.text(Msg::ConsoleActionReplayPlayPause).to_owned(),
            Self::ReplayStepBack => l.text(Msg::ConsoleActionReplayStepBack).to_owned(),
            Self::ReplayStepForward => l.text(Msg::ConsoleActionReplayStepForward).to_owned(),
            Self::ReplayShuttleBack => l.text(Msg::ConsoleActionReplayShuttleBack).to_owned(),
            Self::ReplayShuttleForward => l.text(Msg::ConsoleActionReplayShuttleForward).to_owned(),
            Self::ReplayPlayPauseB => l.text(Msg::ConsoleActionReplayPlayPauseB).to_owned(),
            Self::ReplayStepBackB => l.text(Msg::ConsoleActionReplayStepBackB).to_owned(),
            Self::ReplayStepForwardB => l.text(Msg::ConsoleActionReplayStepForwardB).to_owned(),
            Self::ReplayShuttleBackB => l.text(Msg::ConsoleActionReplayShuttleBackB).to_owned(),
            Self::ReplayShuttleForwardB => {
                l.text(Msg::ConsoleActionReplayShuttleForwardB).to_owned()
            }
        }
    }
}

/// One key press: a key with Ctrl (Cmd on macOS), Shift, and Alt held
/// exactly as given.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConsoleStroke {
    pub key: Key,
    pub command: bool,
    pub shift: bool,
    pub alt: bool,
}

impl ConsoleStroke {
    /// `key` alone.
    #[must_use]
    pub const fn plain(key: Key) -> Self {
        Self {
            key,
            command: false,
            shift: false,
            alt: false,
        }
    }

    /// `key` with Ctrl (Cmd on macOS).
    #[must_use]
    pub const fn command(key: Key) -> Self {
        Self {
            command: true,
            ..Self::plain(key)
        }
    }

    /// `key` with Shift.
    #[must_use]
    pub const fn shift(key: Key) -> Self {
        Self {
            shift: true,
            ..Self::plain(key)
        }
    }

    /// `key` with Alt.
    #[must_use]
    pub const fn alt(key: Key) -> Self {
        Self {
            alt: true,
            ..Self::plain(key)
        }
    }

    /// The modifiers a press must hold, matched exactly.
    #[must_use]
    pub const fn modifiers(self) -> Modifiers {
        Modifiers {
            alt: self.alt,
            ctrl: false,
            shift: self.shift,
            mac_cmd: false,
            command: self.command,
        }
    }

    /// Whether a press of `key` with `pressed` modifiers is this stroke.
    #[must_use]
    pub fn matches(self, key: Key, pressed: Modifiers) -> bool {
        key == self.key && pressed.matches_exact(self.modifiers())
    }

    /// `Ctrl+Shift+Alt+F9`: the stroke as the console writes a key.
    #[must_use]
    pub fn text(self) -> String {
        let mut text = String::new();
        for (held, name) in [
            (self.command, "Ctrl"),
            (self.shift, "Shift"),
            (self.alt, "Alt"),
        ] {
            if held {
                text.push_str(name);
                text.push('+');
            }
        }
        text.push_str(self.key.name());
        text
    }

    /// Parses [`Self::text`]: modifiers in that order, then a key name.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts: Vec<&str> = text.split('+').collect();
        // `Ctrl++` names the Plus key.
        if text.ends_with("++") {
            parts.truncate(parts.len().saturating_sub(2));
            parts.push("+");
        }
        let (name, held) = parts.split_last()?;
        let mut stroke = Self::plain(Key::from_name(name)?);
        let mut order = 0;
        for modifier in held {
            let (rank, flag) = match *modifier {
                "Ctrl" => (1, &mut stroke.command),
                "Shift" => (2, &mut stroke.shift),
                "Alt" => (3, &mut stroke.alt),
                _ => return None,
            };
            if rank <= order {
                return None;
            }
            order = rank;
            *flag = true;
        }
        (stroke.text() == text).then_some(stroke)
    }

    /// Whether the stroke presses the focused control while one has
    /// keyboard focus (Space and Enter alone).
    #[must_use]
    pub const fn presses_focus(self) -> bool {
        matches!(self.key, Key::Space | Key::Enter) && !self.command && !self.shift && !self.alt
    }
}

/// A profile that ships with the console.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum BuiltinProfile {
    /// The console's own keys.
    #[default]
    FreeMix,
    /// The layout vMix operators commonly give vMix's shortcut functions.
    Vmix,
}

impl BuiltinProfile {
    pub const ALL: [Self; 2] = [Self::FreeMix, Self::Vmix];

    /// The profile's name in the preferences file.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::FreeMix => "freemix",
            Self::Vmix => "vmix",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|profile| profile.token() == token)
    }

    /// The profile's name.
    #[must_use]
    pub const fn title(self) -> Msg {
        match self {
            Self::FreeMix => Msg::KeyProfileFreeMix,
            Self::Vmix => Msg::KeyProfileVmix,
        }
    }

    /// What the profile is, for hover help.
    #[must_use]
    pub const fn hover(self) -> Msg {
        match self {
            Self::FreeMix => Msg::KeyProfileFreeMixHover,
            Self::Vmix => Msg::KeyProfileVmixHover,
        }
    }

    /// The profile's keys.
    #[must_use]
    #[allow(clippy::too_many_lines)] // one binding per console action
    pub fn keymap(self) -> ConsoleKeymap {
        let digit = |n: u8| match n {
            1 => Key::Num1,
            2 => Key::Num2,
            3 => Key::Num3,
            4 => Key::Num4,
            5 => Key::Num5,
            6 => Key::Num6,
            7 => Key::Num7,
            8 => Key::Num8,
            _ => Key::Num9,
        };
        let mut bindings = Vec::new();
        match self {
            Self::FreeMix => {
                for n in 1..=8 {
                    bindings.push((ConsoleAction::Preview(n), ConsoleStroke::plain(digit(n))));
                }
                bindings.extend([
                    (ConsoleAction::Cut, ConsoleStroke::plain(Key::C)),
                    (ConsoleAction::Fade, ConsoleStroke::plain(Key::F)),
                    (ConsoleAction::FadeToBlack, ConsoleStroke::plain(Key::B)),
                ]);
                for (n, key) in [(1, Key::F5), (2, Key::F6), (3, Key::F7), (4, Key::F8)] {
                    bindings.push((ConsoleAction::Favorite(n), ConsoleStroke::plain(key)));
                }
                bindings.push((ConsoleAction::CueGo, ConsoleStroke::plain(Key::G)));
                bindings.extend([
                    (
                        ConsoleAction::SlideNext,
                        ConsoleStroke::shift(Key::ArrowRight),
                    ),
                    (
                        ConsoleAction::SlidePrevious,
                        ConsoleStroke::shift(Key::ArrowLeft),
                    ),
                ]);
                bindings.extend([
                    (ConsoleAction::ReplayMarkIn, ConsoleStroke::plain(Key::I)),
                    (ConsoleAction::ReplayMarkOut, ConsoleStroke::plain(Key::O)),
                    (ConsoleAction::ReplayCapture, ConsoleStroke::plain(Key::E)),
                    (ConsoleAction::ReplayPlayPause, ConsoleStroke::plain(Key::K)),
                    (
                        ConsoleAction::ReplayStepBack,
                        ConsoleStroke::plain(Key::ArrowLeft),
                    ),
                    (
                        ConsoleAction::ReplayStepForward,
                        ConsoleStroke::plain(Key::ArrowRight),
                    ),
                    (
                        ConsoleAction::ReplayShuttleBack,
                        ConsoleStroke::plain(Key::J),
                    ),
                    (
                        ConsoleAction::ReplayShuttleForward,
                        ConsoleStroke::plain(Key::L),
                    ),
                ]);
            }
            Self::Vmix => {
                for n in 1..=NUMBERED_INPUTS {
                    bindings.push((ConsoleAction::Preview(n), ConsoleStroke::plain(digit(n))));
                    bindings.push((ConsoleAction::Take(n), ConsoleStroke::command(digit(n))));
                    bindings.push((ConsoleAction::QuickPlay(n), ConsoleStroke::alt(digit(n))));
                }
                bindings.extend([
                    (ConsoleAction::Cut, ConsoleStroke::plain(Key::Space)),
                    (ConsoleAction::Fade, ConsoleStroke::plain(Key::Enter)),
                    (ConsoleAction::FadeToBlack, ConsoleStroke::command(Key::B)),
                ]);
                for (n, key) in [(1, Key::F1), (2, Key::F2), (3, Key::F3), (4, Key::F4)] {
                    bindings.push((ConsoleAction::Favorite(n), ConsoleStroke::command(key)));
                }
                for (n, key) in [(1, Key::F9), (2, Key::F10), (3, Key::F11), (4, Key::F12)] {
                    bindings.push((ConsoleAction::Overlay(n), ConsoleStroke::plain(key)));
                }
                bindings.extend([(ConsoleAction::CueGo, ConsoleStroke::plain(Key::G))]);
                bindings.extend([
                    (
                        ConsoleAction::SlideNext,
                        ConsoleStroke::shift(Key::ArrowRight),
                    ),
                    (
                        ConsoleAction::SlidePrevious,
                        ConsoleStroke::shift(Key::ArrowLeft),
                    ),
                ]);
                bindings.extend([
                    (ConsoleAction::ReplayMarkIn, ConsoleStroke::plain(Key::I)),
                    (ConsoleAction::ReplayMarkOut, ConsoleStroke::plain(Key::O)),
                    (ConsoleAction::ReplayCapture, ConsoleStroke::plain(Key::E)),
                    (ConsoleAction::ReplayPlayPause, ConsoleStroke::plain(Key::K)),
                    (
                        ConsoleAction::ReplayStepBack,
                        ConsoleStroke::plain(Key::ArrowLeft),
                    ),
                    (
                        ConsoleAction::ReplayStepForward,
                        ConsoleStroke::plain(Key::ArrowRight),
                    ),
                    (
                        ConsoleAction::ReplayShuttleBack,
                        ConsoleStroke::plain(Key::J),
                    ),
                    (
                        ConsoleAction::ReplayShuttleForward,
                        ConsoleStroke::plain(Key::L),
                    ),
                ]);
            }
        }
        let mut keymap = ConsoleKeymap { bindings };
        keymap.sort();
        keymap
    }
}

/// The keys one profile gives the console's actions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsoleKeymap {
    /// At most one key per action and one action per key, in
    /// [`ConsoleAction::all`] order.
    bindings: Vec<(ConsoleAction, ConsoleStroke)>,
}

/// The `FreeMix` profile's keys, shared.
pub(crate) fn freemix() -> &'static ConsoleKeymap {
    static FREEMIX: std::sync::LazyLock<ConsoleKeymap> =
        std::sync::LazyLock::new(|| BuiltinProfile::FreeMix.keymap());
    &FREEMIX
}

impl Default for ConsoleKeymap {
    fn default() -> Self {
        BuiltinProfile::FreeMix.keymap()
    }
}

impl ConsoleKeymap {
    /// The action a press of `key` with `pressed` modifiers fires, if any.
    #[must_use]
    pub fn action(&self, key: Key, pressed: Modifiers) -> Option<ConsoleAction> {
        self.bindings
            .iter()
            .find(|(_, stroke)| stroke.matches(key, pressed))
            .map(|(action, _)| *action)
    }

    /// The key of `action`, if the profile gives it one.
    #[must_use]
    pub fn stroke(&self, action: ConsoleAction) -> Option<ConsoleStroke> {
        self.bindings
            .iter()
            .find(|(bound, _)| *bound == action)
            .map(|(_, stroke)| *stroke)
    }

    /// Every action with a key, in [`ConsoleAction::all`] order.
    #[must_use]
    pub fn bindings(&self) -> &[(ConsoleAction, ConsoleStroke)] {
        &self.bindings
    }

    /// The action already on `stroke`, if one is.
    #[must_use]
    pub fn owner(&self, stroke: ConsoleStroke) -> Option<ConsoleAction> {
        self.bindings
            .iter()
            .find(|(_, bound)| *bound == stroke)
            .map(|(action, _)| *action)
    }

    /// `action`'s description with its key, as hover help writes a key
    /// (`Cut  [C]`), or alone when the profile gives it none.
    #[must_use]
    pub fn hint(&self, l: Locale, text: &str, action: ConsoleAction) -> String {
        self.stroke(action).map_or_else(
            || text.to_owned(),
            |stroke| crate::l10n::with_key(l, text, &stroke.text()),
        )
    }

    /// Gives `action` the key `stroke` (or none), as the profile editor
    /// does; the caller has checked the key is free.
    fn set(&mut self, action: ConsoleAction, stroke: Option<ConsoleStroke>) {
        self.bindings.retain(|(bound, _)| *bound != action);
        if let Some(stroke) = stroke {
            self.bindings.push((action, stroke));
        }
        self.sort();
    }

    /// Puts the bindings in [`ConsoleAction::all`] order.
    fn sort(&mut self) {
        let order = ConsoleAction::all();
        self.bindings.sort_by_key(|(bound, _)| {
            order
                .iter()
                .position(|action| action == bound)
                .unwrap_or(usize::MAX)
        });
    }

    /// [`Self::reference_rows`] of the actions the reference lists under
    /// `group`.
    #[must_use]
    pub(crate) fn reference_rows_in(
        &self,
        l: Locale,
        group: crate::ShortcutGroup,
    ) -> Vec<(String, String)> {
        Self {
            bindings: self
                .bindings
                .iter()
                .filter(|(action, _)| action.group() == group)
                .copied()
                .collect(),
        }
        .reference_rows(l)
    }

    /// The reference rows of these keys: a run of numbered actions on keys
    /// with the same modifiers and consecutive digits reads `1 – 9` beside
    /// one description, every other bound action its own row. Each row is
    /// `(keys, action)`.
    #[must_use]
    pub fn reference_rows(&self, l: Locale) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        let mut index = 0;
        while index < self.bindings.len() {
            let (action, stroke) = self.bindings[index];
            let run = self.bindings[index..]
                .iter()
                .enumerate()
                .take_while(|(offset, (next, next_stroke))| {
                    let offset = u8::try_from(*offset).unwrap_or(u8::MAX);
                    same_kind(action, *next, offset)
                        && digit_of(next_stroke.key)
                            .zip(digit_of(stroke.key))
                            .is_some_and(|(next, first)| next == first.saturating_add(offset))
                        && ConsoleStroke {
                            key: stroke.key,
                            ..*next_stroke
                        } == stroke
                })
                .count()
                .max(1);
            if run >= 3 {
                let (_, last) = self.bindings[index + run - 1];
                rows.push((
                    format!("{} – {}", stroke.text(), last.key.name()),
                    run_label(l, action),
                ));
            } else {
                for (action, stroke) in &self.bindings[index..index + run] {
                    rows.push((stroke.text(), action.label(l)));
                }
            }
            index += run;
        }
        rows
    }
}

/// Whether `next` is the action `offset` places after `first` in the same
/// numbered family.
fn same_kind(first: ConsoleAction, next: ConsoleAction, offset: u8) -> bool {
    let step = |a: u8, b: u8| a.checked_add(offset) == Some(b);
    match (first, next) {
        (ConsoleAction::Preview(a), ConsoleAction::Preview(b))
        | (ConsoleAction::Take(a), ConsoleAction::Take(b))
        | (ConsoleAction::QuickPlay(a), ConsoleAction::QuickPlay(b))
        | (ConsoleAction::Favorite(a), ConsoleAction::Favorite(b))
        | (ConsoleAction::Overlay(a), ConsoleAction::Overlay(b)) => step(a, b),
        _ => offset == 0 && first == next,
    }
}

/// The digit or function-key number a key names.
const fn digit_of(key: Key) -> Option<u8> {
    Some(match key {
        Key::Num0 => 0,
        Key::Num1 | Key::F1 => 1,
        Key::Num2 | Key::F2 => 2,
        Key::Num3 | Key::F3 => 3,
        Key::Num4 | Key::F4 => 4,
        Key::Num5 | Key::F5 => 5,
        Key::Num6 | Key::F6 => 6,
        Key::Num7 | Key::F7 => 7,
        Key::Num8 | Key::F8 => 8,
        Key::Num9 | Key::F9 => 9,
        Key::F10 => 10,
        Key::F11 => 11,
        Key::F12 => 12,
        _ => return None,
    })
}

/// The description of a run of numbered actions.
fn run_label(l: Locale, action: ConsoleAction) -> String {
    l.text(match action {
        ConsoleAction::Preview(_) => Msg::ConsoleActionPreviewRun,
        ConsoleAction::Take(_) => Msg::ConsoleActionTakeRun,
        ConsoleAction::QuickPlay(_) => Msg::ConsoleActionQuickPlayRun,
        ConsoleAction::Overlay(_) => Msg::ConsoleActionOverlayRun,
        _ => Msg::ShortcutFavorite,
    })
    .to_owned()
}

/// One profile of the operator's own: a built-in profile with some
/// actions given other keys or none.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomProfile {
    pub name: String,
    pub base: BuiltinProfile,
    /// The actions whose key differs from the base profile's.
    changes: BTreeMap<ConsoleAction, Option<ConsoleStroke>>,
}

impl CustomProfile {
    /// A copy of `base` named `name`.
    #[must_use]
    pub fn new(name: impl Into<String>, base: BuiltinProfile) -> Self {
        Self {
            name: name.into(),
            base,
            changes: BTreeMap::new(),
        }
    }

    /// The profile's keys.
    #[must_use]
    pub fn keymap(&self) -> ConsoleKeymap {
        let mut keymap = self.base.keymap();
        for (action, stroke) in &self.changes {
            keymap.set(*action, *stroke);
        }
        keymap
    }

    /// Whether `action` has another key here than in the base profile.
    #[must_use]
    pub fn changed(&self, action: ConsoleAction) -> bool {
        self.changes.contains_key(&action)
    }
}

/// The profile the console answers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileChoice {
    Builtin(BuiltinProfile),
    /// The operator's profile at this index.
    Custom(usize),
}

impl Default for ProfileChoice {
    fn default() -> Self {
        Self::Builtin(BuiltinProfile::FreeMix)
    }
}

/// Why a key cannot go to an action in a profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyRefusal {
    /// The console keeps the key outside every profile (its reference
    /// text).
    Fixed(&'static str),
    /// Another action of the profile has the key.
    Taken(ConsoleAction),
    /// A built-in profile cannot change.
    Builtin,
    /// No such profile.
    Missing,
}

/// The console's profiles: the built-in ones, the operator's, and which
/// one the console answers.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConsoleKeyProfiles {
    active: ProfileChoice,
    custom: Vec<CustomProfile>,
    /// This console's MIDI controller bindings (RC-005), saved with the
    /// profiles.
    pub midi: crate::midi_bindings::MidiBindings,
    /// This console's HID controller bindings (RC-005), saved with the
    /// profiles.
    pub hid: crate::hid_bindings::HidBindings,
}

impl ConsoleKeyProfiles {
    /// The profile the console answers.
    #[must_use]
    pub const fn active(&self) -> ProfileChoice {
        self.active
    }

    /// The keys of the profile the console answers.
    #[must_use]
    pub fn active_keymap(&self) -> ConsoleKeymap {
        self.keymap(self.active)
            .unwrap_or_else(|| BuiltinProfile::FreeMix.keymap())
    }

    /// The keys of `choice`, if it names a profile.
    #[must_use]
    pub fn keymap(&self, choice: ProfileChoice) -> Option<ConsoleKeymap> {
        match choice {
            ProfileChoice::Builtin(profile) => Some(profile.keymap()),
            ProfileChoice::Custom(index) => self.custom.get(index).map(CustomProfile::keymap),
        }
    }

    /// The operator's profiles, in order.
    #[must_use]
    pub fn custom(&self) -> &[CustomProfile] {
        &self.custom
    }

    /// The name `choice` shows.
    #[must_use]
    pub fn name(&self, l: Locale, choice: ProfileChoice) -> String {
        match choice {
            ProfileChoice::Builtin(profile) => l.text(profile.title()).to_owned(),
            ProfileChoice::Custom(index) => self
                .custom
                .get(index)
                .map_or_else(String::new, |profile| profile.name.clone()),
        }
    }

    /// Makes the console answer `choice`; `false` when it names no profile.
    pub fn choose(&mut self, choice: ProfileChoice) -> bool {
        if self.keymap(choice).is_none() {
            return false;
        }
        self.active = choice;
        true
    }

    /// Adds a copy of `from` named `name` and returns its choice, or `None`
    /// at [`MAX_CUSTOM_PROFILES`], for an unusable name, or when `from`
    /// names no profile.
    pub fn add(&mut self, name: &str, from: ProfileChoice) -> Option<ProfileChoice> {
        if self.custom.len() >= MAX_CUSTOM_PROFILES || !profile_name_is_valid(name) {
            return None;
        }
        let profile = match from {
            ProfileChoice::Builtin(base) => CustomProfile::new(name, base),
            ProfileChoice::Custom(index) => CustomProfile {
                name: name.to_owned(),
                ..self.custom.get(index)?.clone()
            },
        };
        self.custom.push(profile);
        Some(ProfileChoice::Custom(self.custom.len() - 1))
    }

    /// Removes the operator's profile `index`; the console answers `FreeMix`
    /// when that was the active one.
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.custom.len() {
            return false;
        }
        self.custom.remove(index);
        self.active = match self.active {
            ProfileChoice::Custom(active) if active == index => ProfileChoice::default(),
            ProfileChoice::Custom(active) if active > index => ProfileChoice::Custom(active - 1),
            other => other,
        };
        true
    }

    /// Renames the operator's profile `index`; `false` for an unusable name.
    pub fn rename(&mut self, index: usize, name: &str) -> bool {
        match self.custom.get_mut(index) {
            Some(profile) if profile_name_is_valid(name) => {
                name.clone_into(&mut profile.name);
                true
            }
            _ => false,
        }
    }

    /// Gives `action` the key `stroke` (or none) in the operator's profile
    /// `index`.
    ///
    /// # Errors
    ///
    /// [`KeyRefusal`] when the key is the console's own, another action of
    /// the profile has it, or `index` names no profile.
    pub fn set_key(
        &mut self,
        index: usize,
        action: ConsoleAction,
        stroke: Option<ConsoleStroke>,
    ) -> Result<(), KeyRefusal> {
        let profile = self.custom.get_mut(index).ok_or(KeyRefusal::Missing)?;
        if let Some(stroke) = stroke {
            if let Some(keys) = crate::user_shortcuts::fixed_console_key(stroke) {
                return Err(KeyRefusal::Fixed(keys));
            }
            if let Some(owner) = profile
                .keymap()
                .owner(stroke)
                .filter(|owner| *owner != action)
            {
                return Err(KeyRefusal::Taken(owner));
            }
        }
        if profile.base.keymap().stroke(action) == stroke {
            profile.changes.remove(&action);
        } else {
            profile.changes.insert(action, stroke);
        }
        Ok(())
    }

    /// Returns `action` to its base profile's key in the operator's profile
    /// `index`.
    ///
    /// # Errors
    ///
    /// [`KeyRefusal::Taken`] when another action has taken that key since.
    pub fn reset_key(&mut self, index: usize, action: ConsoleAction) -> Result<(), KeyRefusal> {
        let base = self
            .custom
            .get(index)
            .ok_or(KeyRefusal::Missing)?
            .base
            .keymap()
            .stroke(action);
        self.set_key(index, action, base)
    }

    /// The preference lines that reproduce these profiles, in order.
    #[must_use]
    pub fn preference_entries(&self) -> Vec<(String, String)> {
        let mut entries = Vec::new();
        if self.active != ProfileChoice::default() {
            let value = match self.active {
                ProfileChoice::Builtin(profile) => profile.token().to_owned(),
                ProfileChoice::Custom(index) => format!("custom.{}", index + 1),
            };
            entries.push((PROFILE_KEY.to_owned(), value));
        }
        for (index, profile) in self.custom.iter().enumerate() {
            let prefix = format!("{CUSTOM_PREFIX}{}.", index + 1);
            entries.push((format!("{prefix}name"), profile.name.clone()));
            entries.push((format!("{prefix}base"), profile.base.token().to_owned()));
            for (action, stroke) in &profile.changes {
                entries.push((
                    format!("{prefix}{}", action.token()),
                    stroke.map_or_else(|| NONE.to_owned(), ConsoleStroke::text),
                ));
            }
        }
        entries.extend(self.midi.preference_entries());
        entries.extend(self.hid.preference_entries());
        entries
    }

    /// Takes in one preference line; `false` when `key` is not a key
    /// profile line or `value` does not parse (the line is then ignored).
    /// Lines may come in any order; [`Self::finish_preferences`] drops what
    /// they leave unusable.
    pub fn apply_preference(&mut self, key: &str, value: &str) -> bool {
        if crate::midi_bindings::MidiBindings::is_preference_key(key) {
            return self.midi.apply_preference(key, value);
        }
        if crate::hid_bindings::HidBindings::is_preference_key(key) {
            return self.hid.apply_preference(key, value);
        }
        if key == PROFILE_KEY {
            let choice = match value.strip_prefix("custom.") {
                Some(position) => position
                    .parse::<usize>()
                    .ok()
                    .filter(|position| (1..=MAX_CUSTOM_PROFILES).contains(position))
                    .map(|position| ProfileChoice::Custom(position - 1)),
                None => BuiltinProfile::from_token(value).map(ProfileChoice::Builtin),
            };
            return choice.is_some_and(|choice| {
                self.active = choice;
                true
            });
        }
        let Some((position, field)) = key
            .strip_prefix(CUSTOM_PREFIX)
            .and_then(|rest| rest.split_once('.'))
        else {
            return false;
        };
        let Some(index) = position
            .parse::<usize>()
            .ok()
            .filter(|position| (1..=MAX_CUSTOM_PROFILES).contains(position))
            .map(|position| position - 1)
        else {
            return false;
        };
        while self.custom.len() <= index {
            self.custom
                .push(CustomProfile::new(String::new(), BuiltinProfile::FreeMix));
        }
        let profile = &mut self.custom[index];
        match field {
            "name" if profile_name_is_valid(value) => {
                value.clone_into(&mut profile.name);
                true
            }
            "base" => BuiltinProfile::from_token(value).is_some_and(|base| {
                profile.base = base;
                true
            }),
            token => {
                let Some(action) = ConsoleAction::from_token(token) else {
                    return false;
                };
                let stroke = if value == NONE {
                    None
                } else {
                    match ConsoleStroke::parse(value) {
                        Some(stroke) => Some(stroke),
                        None => return false,
                    }
                };
                profile.changes.insert(action, stroke);
                true
            }
        }
    }

    /// Settles what [`Self::apply_preference`] read: profiles without a
    /// usable name are dropped (and with them the choice of one), changes
    /// equal to the base key are forgotten, and a change that takes a
    /// console key or another action's key is dropped.
    pub fn finish_preferences(&mut self) {
        let mut kept = Vec::new();
        let mut moved = vec![None; self.custom.len()];
        for (index, profile) in std::mem::take(&mut self.custom).into_iter().enumerate() {
            if profile_name_is_valid(&profile.name) {
                moved[index] = Some(kept.len());
                kept.push(profile);
            }
        }
        for profile in &mut kept {
            let base = profile.base.keymap();
            let mut changes: Vec<_> = std::mem::take(&mut profile.changes)
                .into_iter()
                .filter(|(action, stroke)| base.stroke(*action) != *stroke)
                .collect();
            // Every changed action lets go of its base key, so a key moved
            // between two actions, or two swapped keys, read back whichever
            // comes first. A change onto a fixed key or a key another action
            // keeps is forgotten (its action takes its base key back), and
            // the rest are checked again until all fit.
            while let Some(refused) = refused_change(&base, &changes) {
                changes.remove(refused);
            }
            profile.changes = changes.into_iter().collect();
        }
        self.custom = kept;
        if let ProfileChoice::Custom(index) = self.active {
            self.active = moved
                .get(index)
                .copied()
                .flatten()
                .map_or_else(ProfileChoice::default, ProfileChoice::Custom);
        }
    }

    /// The `.fmkeys` text of `choice`: its name, base, and changed keys (a
    /// built-in profile exports as a copy of itself with no changes).
    #[must_use]
    pub fn export_text(&self, l: Locale, choice: ProfileChoice) -> Option<String> {
        let profile = match choice {
            ProfileChoice::Builtin(base) => CustomProfile::new(self.name(l, choice), base),
            ProfileChoice::Custom(index) => self.custom.get(index)?.clone(),
        };
        let mut text = String::from("# FreeMix key profile\n");
        let single = Self {
            active: ProfileChoice::default(),
            custom: vec![profile],
            midi: crate::midi_bindings::MidiBindings::default(),
            hid: crate::hid_bindings::HidBindings::default(),
        };
        for (key, value) in single.preference_entries() {
            let field = key.strip_prefix("keymap.custom.1.")?;
            text.push_str(field);
            text.push_str(" = ");
            text.push_str(&value);
            text.push('\n');
        }
        Some(text)
    }

    /// Adds the profile `.fmkeys` text describes as one of the operator's
    /// own and returns its choice. Unknown or unusable lines are skipped,
    /// as in the preferences file.
    ///
    /// # Errors
    ///
    /// [`KeyFileError::Full`] at [`MAX_CUSTOM_PROFILES`], or
    /// [`KeyFileError::Invalid`] for text too long or without a usable
    /// name.
    pub fn import_text(&mut self, text: &str) -> Result<ProfileChoice, KeyFileError> {
        if self.custom.len() >= MAX_CUSTOM_PROFILES {
            return Err(KeyFileError::Full);
        }
        if text.len() > MAX_KEY_FILE_BYTES {
            return Err(KeyFileError::Invalid);
        }
        let mut read = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                read.apply_preference(&format!("{CUSTOM_PREFIX}1.{}", key.trim()), value.trim());
            }
        }
        read.finish_preferences();
        let profile = read.custom.pop().ok_or(KeyFileError::Invalid)?;
        self.custom.push(profile);
        Ok(ProfileChoice::Custom(self.custom.len() - 1))
    }

    /// Whether `key` is one of this module's preference keys.
    #[must_use]
    pub fn is_preference_key(key: &str) -> bool {
        key.starts_with(PREFIX)
    }
}

/// The first of `changes` (actions' keys other than `base`'s) that does
/// not fit: onto a fixed console key, or a key an action keeps from `base`
/// or an earlier change takes.
fn refused_change(
    base: &ConsoleKeymap,
    changes: &[(ConsoleAction, Option<ConsoleStroke>)],
) -> Option<usize> {
    let mut keymap = base.clone();
    for (action, _) in changes {
        keymap.set(*action, None);
    }
    for (index, (action, stroke)) in changes.iter().enumerate() {
        let Some(stroke) = stroke else {
            continue;
        };
        if crate::user_shortcuts::fixed_console_key(*stroke).is_some()
            || keymap.owner(*stroke).is_some()
        {
            return Some(index);
        }
        keymap.set(*action, Some(*stroke));
    }
    None
}

/// Longest `.fmkeys` file read.
pub const MAX_KEY_FILE_BYTES: usize = 64 * 1024;

/// Why a `.fmkeys` file adds no profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyFileError {
    /// The operator already has [`MAX_CUSTOM_PROFILES`] profiles.
    Full,
    /// The file is too long or names no usable profile.
    Invalid,
}

/// Whether `name` is a usable custom profile name: non-blank, at most
/// [`MAX_PROFILE_NAME_BYTES`], and free of control characters.
#[must_use]
pub fn profile_name_is_valid(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_PROFILE_NAME_BYTES
        && !name.chars().any(char::is_control)
}

#[cfg(test)]
mod tests;
