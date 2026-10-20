//! Local console preferences that survive a restart: the UI language, the
//! top-bar clock, the UI scale and high-contrast theme, the animation
//! timeline's Snapping toggle, this console's own keys for project
//! shortcuts (RC-004), and the panel layout (the docked panels, popped-out
//! windows and their places, tool windows, and the operator's saved
//! layouts).
//!
//! The file is plain `key = value` text in the platform configuration
//! directory (`$XDG_CONFIG_HOME/freemix/studio.conf`, else
//! `~/.config/freemix/studio.conf`; `~/Library/Application Support/FreeMix`
//! on macOS; `%APPDATA%\FreeMix` on Windows), or the path in
//! `FREEMIX_STUDIO_PREFS`. Preferences are local to this console and never
//! sent to the daemon. Unknown keys and unparsable values are ignored, so a
//! hand-edited or newer file never stops Studio from starting.

use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fm_ui_egui::{Appearance, ConsoleLayout, ShortcutKeyOverrides};
use fm_ui_model::i18n::{HourCycle, Language, Locale};

/// Environment variable naming the preferences file explicitly.
pub const PREFERENCES_ENV: &str = "FREEMIX_STUDIO_PREFS";

const FILE_NAME: &str = "studio.conf";

/// The key of the animation timeline's Snapping toggle.
const SNAPPING_KEY: &str = "animation.snapping";

/// The key prefix of a console key for a project shortcut.
const SHORTCUT_KEY_PREFIX: &str = "shortcut.key.";

/// How long the preferences must stay unchanged before they are written,
/// so dragging a splitter or a window writes the file once, not every
/// frame.
pub const SAVE_DELAY: Duration = Duration::from_millis(500);

/// Preferences saved from an earlier session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SavedPreferences {
    pub language: Option<Language>,
    pub hour_cycle: Option<HourCycle>,
    /// The panel layout; `None` when the file has no layout keys.
    pub layout: Option<ConsoleLayout>,
    /// The UI scale in percent, clamped to the supported range.
    pub ui_scale_percent: Option<u16>,
    /// Whether the high-contrast theme is on.
    pub high_contrast: Option<bool>,
    /// Whether the animation timelines snap drags without Shift (their
    /// Snapping toggle).
    pub timeline_snapping: Option<bool>,
    /// This console's own keys for project shortcuts; `None` when the
    /// file has none.
    pub shortcut_keys: Option<ShortcutKeyOverrides>,
}

impl SavedPreferences {
    /// The preferences that reproduce `locale`.
    #[must_use]
    pub const fn from_locale(locale: Locale) -> Self {
        Self {
            language: Some(locale.language),
            hour_cycle: Some(locale.hour_cycle),
            layout: None,
            ui_scale_percent: None,
            high_contrast: None,
            timeline_snapping: None,
            shortcut_keys: None,
        }
    }

    /// The Snapping toggle these preferences select (on by default).
    #[must_use]
    pub fn timeline_snapping(&self) -> bool {
        self.timeline_snapping.unwrap_or(true)
    }

    /// The appearance these preferences select, defaults filling the gaps.
    #[must_use]
    pub fn appearance(&self) -> Appearance {
        let default = Appearance::default();
        Appearance {
            ui_scale_percent: self.ui_scale_percent.unwrap_or(default.ui_scale_percent),
            high_contrast: self.high_contrast.unwrap_or(default.high_contrast),
        }
    }

    /// Parses the file text; unknown keys, comments, and bad values are
    /// skipped. A layout entry this version does not write puts the
    /// default layout in its place, flagged
    /// [`ConsoleLayout::rejected`] so Studio says so.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut saved = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "language" => saved.language = Language::from_tag(value).or(saved.language),
                "clock" => saved.hour_cycle = HourCycle::from_flag(value).or(saved.hour_cycle),
                "ui.scale" => {
                    saved.ui_scale_percent =
                        Appearance::parse_ui_scale(value).or(saved.ui_scale_percent);
                }
                "ui.high_contrast" => {
                    saved.high_contrast =
                        Appearance::parse_high_contrast(value).or(saved.high_contrast);
                }
                SNAPPING_KEY => {
                    saved.timeline_snapping = match value {
                        "on" => Some(true),
                        "off" => Some(false),
                        _ => saved.timeline_snapping,
                    };
                }
                key if key.starts_with(SHORTCUT_KEY_PREFIX) => {
                    let mut overrides = saved.shortcut_keys.take().unwrap_or_default();
                    overrides.apply_preference(key, value);
                    saved.shortcut_keys = (!overrides.is_empty()).then_some(overrides);
                }
                key => {
                    let mut layout = saved.layout.clone().unwrap_or_default();
                    if layout.apply_preference(key, value) {
                        saved.layout = Some(layout);
                    }
                }
            }
        }
        saved
    }

    /// The file text for these preferences.
    #[must_use]
    pub fn render(&self) -> String {
        let mut text = String::from("# FreeMix Studio console preferences (local only)\n");
        if let Some(language) = self.language {
            text.push_str("language = ");
            text.push_str(language.code());
            text.push('\n');
        }
        if let Some(hour_cycle) = self.hour_cycle {
            text.push_str(match hour_cycle {
                HourCycle::H23 => "clock = 24h\n",
                HourCycle::H12 => "clock = 12h\n",
            });
        }
        if let Some(percent) = self.ui_scale_percent {
            text.push_str("ui.scale = ");
            text.push_str(&Appearance::clamp_ui_scale(i64::from(percent)).to_string());
            text.push('\n');
        }
        if let Some(high_contrast) = self.high_contrast {
            text.push_str(if high_contrast {
                "ui.high_contrast = on\n"
            } else {
                "ui.high_contrast = off\n"
            });
        }
        if let Some(snapping) = self.timeline_snapping {
            text.push_str(SNAPPING_KEY);
            text.push_str(if snapping { " = on\n" } else { " = off\n" });
        }
        if let Some(overrides) = &self.shortcut_keys {
            for (key, value) in overrides.preference_entries() {
                text.push_str(&key);
                text.push_str(" = ");
                text.push_str(&value);
                text.push('\n');
            }
        }
        if let Some(layout) = &self.layout {
            for (key, value) in layout.preference_entries() {
                text.push_str(key);
                text.push_str(" = ");
                text.push_str(&value);
                text.push('\n');
            }
        }
        text
    }

    /// Reads the preferences at `path`; a missing file is no preferences.
    ///
    /// # Errors
    ///
    /// Returns read errors other than a missing file.
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    /// Writes the preferences to `path`, creating its directory, through a
    /// temporary file so a crash never leaves a half-written file.
    ///
    /// # Errors
    ///
    /// Returns directory creation, write, or rename errors.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("conf.tmp");
        std::fs::write(&temporary, self.render())?;
        std::fs::rename(&temporary, path)
    }
}

/// The preferences file for this platform, from the environment lookup
/// `env`; `None` when no home or configuration directory is known.
#[must_use]
pub fn preferences_path(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let set = |name: &str| env(name).filter(|value| !value.is_empty());
    if let Some(path) = set(PREFERENCES_ENV) {
        return Some(PathBuf::from(path));
    }
    let directory = if cfg!(windows) {
        set("APPDATA").map(|base| PathBuf::from(base).join("FreeMix"))
    } else if cfg!(target_os = "macos") {
        set("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("FreeMix")
        })
    } else {
        set("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|base| base.is_absolute())
            .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|base| base.join("freemix"))
    }?;
    Some(directory.join(FILE_NAME))
}

/// Saves the console preferences whenever the operator changes them.
///
/// Each preference is written only once the operator changes that very
/// preference, so a `--lang` or `--clock` override for one session is never
/// written back when the operator changes the clock, the layout, the UI
/// scale, or the theme, and vice versa.
///
/// Studio reports the preferences every frame through
/// [`Self::observe_at`], which writes them once they have stayed unchanged
/// for [`SAVE_DELAY`]; [`Self::flush`] writes what is still pending on
/// exit.
#[derive(Debug)]
pub struct PreferenceStore {
    path: Option<PathBuf>,
    /// The preferences as last loaded or saved.
    saved: SavedPreferences,
    /// The locale in effect at startup or last saved.
    known_locale: Locale,
    /// The layout in effect at startup or last saved.
    known_layout: ConsoleLayout,
    /// The appearance in effect at startup or last saved.
    known_appearance: Appearance,
    /// The timeline Snapping toggle at startup or last saved.
    known_snapping: bool,
    /// This console's keys for project shortcuts at startup or last saved.
    known_shortcut_keys: ShortcutKeyOverrides,
    /// When the unsaved change was last seen; `None` when the file is up
    /// to date.
    pending: Option<Instant>,
}

impl PreferenceStore {
    /// A store for `path` whose file held `saved` and whose console started
    /// in `locale` with `layout` and `appearance` (and the Snapping toggle
    /// `saved` selects); nothing is written until one of them changes.
    #[must_use]
    pub fn new(
        path: Option<PathBuf>,
        saved: SavedPreferences,
        locale: Locale,
        layout: ConsoleLayout,
        appearance: Appearance,
    ) -> Self {
        let known_snapping = saved.timeline_snapping();
        let known_shortcut_keys = saved.shortcut_keys.clone().unwrap_or_default();
        Self {
            path,
            saved,
            known_snapping,
            known_shortcut_keys,
            known_locale: locale,
            known_layout: layout,
            known_appearance: appearance,
            pending: None,
        }
    }

    /// Saves whichever of `locale`, `layout`, and `appearance` differ from
    /// the last known ones at once, with anything still pending. Returns
    /// the write error, if any; a failed save is retried on the next call.
    ///
    /// # Errors
    ///
    /// Returns the error from [`SavedPreferences::save`].
    pub fn observe(
        &mut self,
        locale: Locale,
        layout: &ConsoleLayout,
        appearance: Appearance,
    ) -> io::Result<()> {
        self.note(Instant::now(), locale, layout, appearance);
        self.flush()
    }

    /// Notes whichever of `locale`, `layout`, and `appearance` differ from
    /// the last known ones at `now`, and writes the preferences once they
    /// have stayed unchanged for [`SAVE_DELAY`]. Returns how long until a
    /// pending write is due (ask for a frame then), or `None` when nothing
    /// is pending.
    ///
    /// # Errors
    ///
    /// Returns the error from [`SavedPreferences::save`]; the write is
    /// tried again [`SAVE_DELAY`] later.
    pub fn observe_at(
        &mut self,
        now: Instant,
        locale: Locale,
        layout: &ConsoleLayout,
        appearance: Appearance,
    ) -> io::Result<Option<Duration>> {
        self.note(now, locale, layout, appearance);
        let Some(changed) = self.pending else {
            return Ok(None);
        };
        let wait = SAVE_DELAY.saturating_sub(now.saturating_duration_since(changed));
        if !wait.is_zero() {
            return Ok(Some(wait));
        }
        if let Err(error) = self.flush() {
            self.pending = Some(now);
            return Err(error);
        }
        Ok(None)
    }

    /// Notes the timeline Snapping toggle at `now`: a change is saved with
    /// the next [`Self::observe_at`] write (the same debounce as the
    /// layout) or [`Self::flush`].
    pub fn note_timeline_snapping(&mut self, now: Instant, snapping: bool) {
        if snapping != self.known_snapping {
            self.known_snapping = snapping;
            self.saved.timeline_snapping = Some(snapping);
            self.pending = Some(now);
        }
    }

    /// Notes this console's keys for project shortcuts at `now`: a change
    /// is saved like the Snapping toggle's.
    pub fn note_shortcut_keys(&mut self, now: Instant, overrides: &ShortcutKeyOverrides) {
        if *overrides != self.known_shortcut_keys {
            self.known_shortcut_keys = overrides.clone();
            self.saved.shortcut_keys = Some(overrides.clone());
            self.pending = Some(now);
        }
    }

    /// Whether a change is waiting to be written.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Writes a pending change now (on exit).
    ///
    /// # Errors
    ///
    /// Returns the error from [`SavedPreferences::save`]; the change stays
    /// pending.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_none() {
            return Ok(());
        }
        if let Some(path) = &self.path {
            self.saved.save(path)?;
        }
        self.pending = None;
        Ok(())
    }

    /// Takes in whichever preferences changed, marking them pending at
    /// `now`.
    fn note(
        &mut self,
        now: Instant,
        locale: Locale,
        layout: &ConsoleLayout,
        appearance: Appearance,
    ) {
        if locale == self.known_locale
            && *layout == self.known_layout
            && appearance == self.known_appearance
        {
            return;
        }
        if locale.language != self.known_locale.language {
            self.saved.language = Some(locale.language);
        }
        if locale.hour_cycle != self.known_locale.hour_cycle {
            self.saved.hour_cycle = Some(locale.hour_cycle);
        }
        self.known_locale = locale;
        if *layout != self.known_layout {
            self.known_layout = layout.clone();
            self.saved.layout = Some(layout.clone());
        }
        if appearance.ui_scale_percent != self.known_appearance.ui_scale_percent {
            self.saved.ui_scale_percent = Some(appearance.ui_scale_percent);
        }
        if appearance.high_contrast != self.known_appearance.high_contrast {
            self.saved.high_contrast = Some(appearance.high_contrast);
        }
        self.known_appearance = appearance;
        self.pending = Some(now);
    }
}
