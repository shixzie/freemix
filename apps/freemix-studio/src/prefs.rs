//! Local console preferences that survive a restart: the UI language and
//! the top-bar clock.
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

use fm_ui_model::i18n::{HourCycle, Language, Locale};

/// Environment variable naming the preferences file explicitly.
pub const PREFERENCES_ENV: &str = "FREEMIX_STUDIO_PREFS";

const FILE_NAME: &str = "studio.conf";

/// Preferences saved from an earlier session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SavedPreferences {
    pub language: Option<Language>,
    pub hour_cycle: Option<HourCycle>,
}

impl SavedPreferences {
    /// The preferences that reproduce `locale`.
    #[must_use]
    pub const fn from_locale(locale: Locale) -> Self {
        Self {
            language: Some(locale.language),
            hour_cycle: Some(locale.hour_cycle),
        }
    }

    /// Parses the file text; unknown keys, comments, and bad values are
    /// skipped.
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
                _ => {}
            }
        }
        saved
    }

    /// The file text for these preferences.
    #[must_use]
    pub fn render(self) -> String {
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
    pub fn save(self, path: &Path) -> io::Result<()> {
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

/// Saves the console locale whenever the operator changes it.
#[derive(Debug)]
pub struct PreferenceStore {
    path: Option<PathBuf>,
    /// The locale last loaded, chosen at startup, or saved.
    known: Locale,
}

impl PreferenceStore {
    /// A store for `path` whose current locale is `startup`; nothing is
    /// written until the locale changes.
    #[must_use]
    pub const fn new(path: Option<PathBuf>, startup: Locale) -> Self {
        Self {
            path,
            known: startup,
        }
    }

    /// Saves `locale` when it differs from the last known one. Returns the
    /// write error, if any; a failed save is retried on the next change.
    ///
    /// # Errors
    ///
    /// Returns the error from [`SavedPreferences::save`].
    pub fn observe(&mut self, locale: Locale) -> io::Result<()> {
        if locale == self.known {
            return Ok(());
        }
        self.known = locale;
        match &self.path {
            Some(path) => SavedPreferences::from_locale(locale).save(path),
            None => Ok(()),
        }
    }
}
