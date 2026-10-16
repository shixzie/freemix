//! Studio message catalogs and locale-aware formatting.
//!
//! Every translated string has a typed key in [`Msg`]. The English text is
//! compiled in beside each key (`messages.rs`), so English is always
//! complete; other languages are data tables (`es.rs`) whose missing keys
//! fall back to English. Templates use named arguments (`{name}`), and a
//! key may carry `one`/`other` plural forms chosen by its `{count}`
//! argument under the language's plural rule. Unit tests fail when a
//! catalog lacks a key, has a key twice, or disagrees with English on
//! argument names or plural shape.
//!
//! [`Locale`] also owns the number, percentage, dB, byte, time-of-day, and
//! date conventions (`format.rs`), so presentation code never hard-codes a
//! decimal point or a 24-hour clock.
//!
//! The language is a local Studio preference. It never crosses the control
//! protocol and never changes the English text of logs, diagnostics, or
//! command results produced outside the UI.
//!
//! # Right-to-left readiness
//!
//! [`Locale::direction`] reports the reading direction of the active
//! language, and UI code places "leading" and "trailing" content from it
//! rather than assuming left-to-right. Both shipped languages are LTR. The
//! renderer (egui/epaint 0.35) shapes glyphs but has no bidirectional text
//! reordering and its default fonts carry no Arabic or Hebrew glyphs, so an
//! RTL catalog would also need a font and a bidi pass before shipping.

mod es;
mod format;
mod messages;
mod refusal;

use std::collections::HashMap;
use std::sync::OnceLock;

pub use messages::Msg;
pub use refusal::{
    InputNames, bounds_field_name, command_refusal_text, input_use_advice, input_use_sentence,
    refusal_reason_text,
};

/// A language with a Studio message catalog.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Language {
    /// English, the compiled-in base catalog.
    #[default]
    English,
    /// Spanish.
    Spanish,
}

impl Language {
    /// Every language with a catalog, English first.
    pub const ALL: [Self; 2] = [Self::English, Self::Spanish];

    /// The BCP 47 primary language subtag (`en`, `es`).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Spanish => "es",
        }
    }

    /// The language's own name, shown in the language picker so an operator
    /// can find their language whatever the current one is.
    #[must_use]
    pub const fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Spanish => "Español",
        }
    }

    /// The reading direction of this language's script.
    #[must_use]
    pub const fn direction(self) -> TextDirection {
        match self {
            Self::English | Self::Spanish => TextDirection::LeftToRight,
        }
    }

    /// Parses a language tag or POSIX locale name (`es`, `es-MX`,
    /// `es_ES.UTF-8`, `EN`); `None` for languages without a catalog and for
    /// the `C`/`POSIX` locales.
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        let primary = tag
            .split(['-', '_', '.', '@'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|language| language.code() == primary)
    }

    /// The CLDR plural category of `count` in this language. English and
    /// Spanish both use `one` for exactly 1 and `other` otherwise (Spanish's
    /// `many` only applies to compact millions, which Studio never prints).
    #[must_use]
    pub const fn plural(self, count: u64) -> PluralCategory {
        match self {
            Self::English | Self::Spanish => {
                if count == 1 {
                    PluralCategory::One
                } else {
                    PluralCategory::Other
                }
            }
        }
    }

    fn catalog(self) -> Option<&'static [Option<Entry>]> {
        match self {
            Self::English => None,
            Self::Spanish => {
                static INDEX: OnceLock<Vec<Option<Entry>>> = OnceLock::new();
                Some(INDEX.get_or_init(|| index(es::CATALOG)))
            }
        }
    }

    /// This language's raw table, for catalog validation.
    fn table(self) -> &'static [(Msg, Entry)] {
        match self {
            Self::English => &[],
            Self::Spanish => es::CATALOG,
        }
    }
}

/// Builds a key-indexed lookup from a catalog table; a later duplicate
/// never replaces an earlier entry (tests reject duplicates outright).
fn index(table: &[(Msg, Entry)]) -> Vec<Option<Entry>> {
    let mut entries = vec![None; Msg::ALL.len()];
    for (msg, entry) in table {
        let slot = &mut entries[*msg as usize];
        if slot.is_none() {
            *slot = Some(*entry);
        }
    }
    entries
}

/// Reading direction of a script.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TextDirection {
    #[default]
    LeftToRight,
    RightToLeft,
}

/// CLDR plural category used to pick a message form.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluralCategory {
    One,
    Other,
}

/// Whether times of day use a 24-hour or a 12-hour clock.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HourCycle {
    /// `00:00:00`–`23:59:59`, the broadcast convention and Studio default.
    #[default]
    H23,
    /// `12:00:00 AM`–`11:59:59 PM` with the language's day-period marker.
    H12,
}

impl HourCycle {
    /// Parses `24h`/`24` or `12h`/`12`.
    #[must_use]
    pub fn from_flag(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "24h" | "24" => Some(Self::H23),
            "12h" | "12" => Some(Self::H12),
            _ => None,
        }
    }
}

/// One catalog entry: a template, or `one`/`other` plural templates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Entry {
    Text(&'static str),
    Plural {
        one: &'static str,
        other: &'static str,
    },
}

impl Entry {
    /// The `other` form (the only form of a plain text entry).
    #[must_use]
    pub const fn other(self) -> &'static str {
        match self {
            Self::Text(text) | Self::Plural { other: text, .. } => text,
        }
    }

    /// Every form of this entry.
    #[must_use]
    pub fn forms(self) -> Vec<&'static str> {
        match self {
            Self::Text(text) => vec![text],
            Self::Plural { one, other } => vec![one, other],
        }
    }

    fn select(self, language: Language, count: Option<u64>) -> &'static str {
        match self {
            Self::Text(text) => text,
            Self::Plural { one, other } => match count.map(|count| language.plural(count)) {
                Some(PluralCategory::One) => one,
                _ => other,
            },
        }
    }
}

/// A plain catalog entry.
pub(crate) const fn t(text: &'static str) -> Entry {
    Entry::Text(text)
}

/// A plural catalog entry selected by the `{count}` argument.
pub(crate) const fn plural(one: &'static str, other: &'static str) -> Entry {
    Entry::Plural { one, other }
}

/// A named template argument value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Arg<'a> {
    /// Text inserted verbatim (names, identifiers, preformatted values).
    Str(&'a str),
    /// A count, grouped by the locale; also selects plural forms.
    Count(u64),
    /// A digit string without grouping (revisions, ordinals, identifiers).
    Plain(u64),
}

impl<'a> From<&'a str> for Arg<'a> {
    fn from(value: &'a str) -> Self {
        Self::Str(value)
    }
}

impl<'a> From<&'a String> for Arg<'a> {
    fn from(value: &'a String) -> Self {
        Self::Str(value)
    }
}

impl From<u64> for Arg<'_> {
    fn from(value: u64) -> Self {
        Self::Count(value)
    }
}

impl From<usize> for Arg<'_> {
    fn from(value: usize) -> Self {
        Self::Count(value as u64)
    }
}

impl From<u32> for Arg<'_> {
    fn from(value: u32) -> Self {
        Self::Count(u64::from(value))
    }
}

impl From<u16> for Arg<'_> {
    fn from(value: u16) -> Self {
        Self::Count(u64::from(value))
    }
}

/// The active UI language plus its formatting preferences.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Locale {
    pub language: Language,
    pub hour_cycle: HourCycle,
}

impl Locale {
    /// English with a 24-hour clock.
    pub const ENGLISH: Self = Self::new(Language::English);

    /// A locale for `language` with the broadcast 24-hour clock.
    #[must_use]
    pub const fn new(language: Language) -> Self {
        Self {
            language,
            hour_cycle: HourCycle::H23,
        }
    }

    /// The same language with another clock.
    #[must_use]
    pub const fn with_hour_cycle(self, hour_cycle: HourCycle) -> Self {
        Self {
            language: self.language,
            hour_cycle,
        }
    }

    /// The reading direction of the active language.
    #[must_use]
    pub const fn direction(self) -> TextDirection {
        self.language.direction()
    }

    /// The entry for `msg`, falling back to English when the active catalog
    /// lacks it.
    #[must_use]
    pub fn entry(self, msg: Msg) -> Entry {
        self.language
            .catalog()
            .and_then(|catalog| catalog[msg as usize])
            .unwrap_or_else(|| msg.source())
    }

    /// The text of an argument-free message.
    #[must_use]
    pub fn text(self, msg: Msg) -> &'static str {
        let text = self.entry(msg).other();
        debug_assert!(
            !text.contains('{') || placeholders(text).is_ok_and(|names| names.is_empty()),
            "{} takes arguments; use Locale::format",
            msg.id()
        );
        text
    }

    /// Formats `msg` with named arguments; a plural message picks its form
    /// from the `count` argument.
    #[must_use]
    pub fn format(self, msg: Msg, args: &[(&str, Arg<'_>)]) -> String {
        let count = args.iter().find_map(|(name, value)| match (*name, value) {
            ("count", Arg::Count(count) | Arg::Plain(count)) => Some(*count),
            _ => None,
        });
        let template = self.entry(msg).select(self.language, count);
        self.fill(msg, template, args)
    }

    /// Translates text that was produced as English by
    /// [`Msg::en`] (for example a disabled-control reason threaded through
    /// as `&'static str`). Unknown text, such as a daemon message, is
    /// returned unchanged.
    #[must_use]
    pub fn tr(self, english: &str) -> &str {
        static REVERSE: OnceLock<HashMap<&'static str, Msg>> = OnceLock::new();
        if self.language == Language::English || english.is_empty() {
            return english;
        }
        REVERSE
            .get_or_init(|| {
                Msg::ALL
                    .iter()
                    .filter_map(|msg| match msg.source() {
                        Entry::Text(text) if !text.contains('{') => Some((text, *msg)),
                        _ => None,
                    })
                    .collect()
            })
            .get(english)
            .map_or(english, |msg| self.text(*msg))
    }

    fn fill(self, msg: Msg, template: &str, args: &[(&str, Arg<'_>)]) -> String {
        let mut out = String::with_capacity(template.len() + 16);
        let mut rest = template;
        while let Some(start) = rest.find(['{', '}']) {
            out.push_str(&rest[..start]);
            let brace = rest.as_bytes()[start];
            let tail = &rest[start + 1..];
            if tail.as_bytes().first() == Some(&brace) {
                // `{{` or `}}` escapes one brace.
                out.push(char::from(brace));
                rest = &tail[1..];
                continue;
            }
            let Some(end) = tail.find('}').filter(|_| brace == b'{') else {
                out.push(char::from(brace));
                rest = tail;
                continue;
            };
            let name = &tail[..end];
            if let Some((_, value)) = args.iter().find(|(arg, _)| *arg == name) {
                self.push_arg(&mut out, *value);
            } else {
                debug_assert!(false, "{} has no argument `{name}`", msg.id());
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
            rest = &tail[end + 1..];
        }
        out.push_str(rest);
        out
    }

    fn push_arg(self, out: &mut String, value: Arg<'_>) {
        match value {
            Arg::Str(text) => out.push_str(text),
            Arg::Count(count) => out.push_str(&self.integer(count)),
            Arg::Plain(value) => out.push_str(&value.to_string()),
        }
    }
}

/// Error from parsing a template's placeholders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateError {
    /// A `{` without a closing `}`.
    Unclosed,
    /// A lone `}` outside a placeholder.
    StrayClose,
    /// A placeholder that is not a lowercase identifier.
    BadName(String),
}

/// The sorted, deduplicated placeholder names of a template.
///
/// # Errors
///
/// Returns an error for unbalanced braces or a malformed placeholder name.
pub fn placeholders(template: &str) -> Result<Vec<String>, TemplateError> {
    let mut names = Vec::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
            }
            '}' => return Err(TemplateError::StrayClose),
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err(TemplateError::Unclosed),
                    }
                }
                if name.is_empty()
                    || !name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                {
                    return Err(TemplateError::BadName(name));
                }
                names.push(name);
            }
            _ => {}
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// A problem found by [`validate_catalogs`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogIssue {
    Missing {
        language: Language,
        key: &'static str,
    },
    Duplicate {
        language: Language,
        key: &'static str,
    },
    BadTemplate {
        language: Language,
        key: &'static str,
        error: TemplateError,
    },
    ArgumentMismatch {
        language: Language,
        key: &'static str,
        expected: Vec<String>,
        found: Vec<String>,
    },
    PluralShape {
        language: Language,
        key: &'static str,
    },
    /// Two English texts collide, so [`Locale::tr`] could not tell them apart.
    AmbiguousEnglish { text: &'static str },
}

/// Checks every catalog against English: each key present exactly once,
/// every form parsing, every form taking exactly English's arguments, and
/// plural keys staying plural (and plain keys plain).
#[must_use]
pub fn validate_catalogs() -> Vec<CatalogIssue> {
    let mut issues = Vec::new();
    let mut english_texts: HashMap<&'static str, Msg> = HashMap::new();
    for msg in Msg::ALL {
        let source = msg.source();
        for form in source.forms() {
            if let Err(error) = placeholders(form) {
                issues.push(CatalogIssue::BadTemplate {
                    language: Language::English,
                    key: msg.id(),
                    error,
                });
            }
        }
        if let Entry::Text(text) = source
            && english_texts.insert(text, *msg).is_some()
        {
            issues.push(CatalogIssue::AmbiguousEnglish { text });
        }
    }
    for language in Language::ALL {
        if language != Language::English {
            issues.extend(validate_table(language, language.table()));
        }
    }
    issues
}

/// Checks one translated catalog table against English.
fn validate_table(language: Language, table: &[(Msg, Entry)]) -> Vec<CatalogIssue> {
    let mut issues = Vec::new();
    for msg in Msg::ALL {
        let key = msg.id();
        let mut entries = table.iter().filter(|(candidate, _)| candidate == msg);
        let Some((_, entry)) = entries.next() else {
            issues.push(CatalogIssue::Missing { language, key });
            continue;
        };
        if entries.next().is_some() {
            issues.push(CatalogIssue::Duplicate { language, key });
        }
        let source = msg.source();
        if matches!(source, Entry::Plural { .. }) != matches!(entry, Entry::Plural { .. }) {
            issues.push(CatalogIssue::PluralShape { language, key });
        }
        let expected = placeholders(source.other()).unwrap_or_default();
        for (form_index, form) in entry.forms().into_iter().enumerate() {
            let found = match placeholders(form) {
                Ok(found) => found,
                Err(error) => {
                    issues.push(CatalogIssue::BadTemplate {
                        language,
                        key,
                        error,
                    });
                    continue;
                }
            };
            // A plural `one` form may spell its count out ("un comando").
            let one_form = form_index == 0 && matches!(entry, Entry::Plural { .. });
            let without_count = |names: &[String]| -> Vec<String> {
                names
                    .iter()
                    .filter(|name| !one_form || *name != "count")
                    .cloned()
                    .collect()
            };
            if without_count(&found) != without_count(&expected) {
                issues.push(CatalogIssue::ArgumentMismatch {
                    language,
                    key,
                    expected: expected.clone(),
                    found,
                });
            }
        }
    }
    issues
}

#[cfg(test)]
mod tests;
