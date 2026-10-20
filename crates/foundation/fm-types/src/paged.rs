//! Paged inputs (IN-014): inputs that show one page of an ordered set at a
//! time and are stepped by the operator.
//!
//! A paged input has a current zero-based page index and a page count. The
//! index is persisted with the input; the count is what its source holds
//! (a PDF's pages, a presentation's slides). Navigation is generic: next,
//! previous, first, last, and go to page N, each resolved against the count
//! by [`PagedNavigation::target`] so every paged kind (documents here, and
//! image slideshows) steps the same way.
//!
//! A document input (a PDF, or a presentation converted to PDF) is the paged
//! kind this module also describes: its [`DocumentFormat`] (from the file
//! name) and the [`DocumentState`] the runtime reports while it loads,
//! converts, and renders it.

use std::fmt;

/// Most pages one paged input steps through.
pub const MAX_PAGED_PAGES: u32 = 10_000;
/// Longest reported document title, in UTF-8 bytes.
pub const MAX_PAGED_TITLE_BYTES: usize = 128;
/// Longest presenter notes text for one page, in UTF-8 bytes.
pub const MAX_PAGED_NOTES_BYTES: usize = 8 * 1024;
/// Longest runtime status message, in UTF-8 bytes.
pub const MAX_PAGED_MESSAGE_BYTES: usize = 160;
/// Most document inputs one show holds.
pub const MAX_DOCUMENT_INPUTS: usize = 16;

/// One navigation step of a paged input.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PagedNavigation {
    /// The following page; the last page stays.
    Next,
    /// The preceding page; the first page stays.
    Previous,
    /// The first page.
    First,
    /// The last page.
    Last,
    /// Zero-based page `n`, clamped to the last page.
    Goto(u32),
}

impl PagedNavigation {
    /// The action's token: `next`, `previous`, `first`, `last`, or `goto`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Next => "next",
            Self::Previous => "previous",
            Self::First => "first",
            Self::Last => "last",
            Self::Goto(_) => "goto",
        }
    }

    /// The step a token names; `page` is the target of `goto` and must be
    /// absent for every other action.
    #[must_use]
    pub fn from_token(token: &str, page: Option<u32>) -> Option<Self> {
        match (token, page) {
            ("next", None) => Some(Self::Next),
            ("previous", None) => Some(Self::Previous),
            ("first", None) => Some(Self::First),
            ("last", None) => Some(Self::Last),
            ("goto", Some(page)) if page < MAX_PAGED_PAGES => Some(Self::Goto(page)),
            _ => None,
        }
    }

    /// The page this step lands on from `current` in a source of `count`
    /// pages. Every result is a valid page (`0..count`); with no pages the
    /// result is `0`.
    #[must_use]
    pub fn target(self, current: u32, count: u32) -> u32 {
        let Some(last) = count.checked_sub(1) else {
            return 0;
        };
        let current = current.min(last);
        match self {
            Self::Next => current.saturating_add(1).min(last),
            Self::Previous => current.saturating_sub(1),
            Self::First => 0,
            Self::Last => last,
            Self::Goto(page) => page.min(last),
        }
    }
}

/// A document file's format, named by its file name's extension.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DocumentFormat {
    /// A PDF, rendered directly.
    Pdf,
    /// A `PowerPoint` presentation (Office Open XML).
    Pptx,
    /// An `OpenDocument` presentation.
    Odp,
    /// A Keynote presentation: never rendered; export it to PDF.
    Keynote,
}

impl DocumentFormat {
    pub const ALL: [Self; 4] = [Self::Pdf, Self::Pptx, Self::Odp, Self::Keynote];

    /// The format's token, which is also its file extension.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Pptx => "pptx",
            Self::Odp => "odp",
            Self::Keynote => "key",
        }
    }

    /// The format a token names.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|format| format.token() == token)
    }

    /// The format a file name's extension names (ASCII case-insensitive).
    #[must_use]
    pub fn from_file_name(name: &str) -> Option<Self> {
        let (_, extension) = name.rsplit_once('.')?;
        Self::from_token(&extension.to_ascii_lowercase())
    }

    /// Whether the format renders only after conversion to PDF.
    #[must_use]
    pub const fn needs_conversion(self) -> bool {
        matches!(self, Self::Pptx | Self::Odp)
    }
}

/// Whether `name` names a document file a document input can show.
#[must_use]
pub fn is_document_file_name(name: &str) -> bool {
    DocumentFormat::from_file_name(name).is_some()
}

/// Where the runtime is with a document input's source.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DocumentState {
    /// The file is being read; the page count may not be known yet.
    #[default]
    Loading,
    /// A presentation is being converted to PDF.
    Converting,
    /// Pages render.
    Ready,
    /// The file cannot render on this server (a presentation without
    /// `LibreOffice`, or a Keynote file); the message says what to do.
    Unavailable,
    /// The file is missing, damaged, encrypted, or failed to convert.
    Failed,
}

impl DocumentState {
    pub const ALL: [Self; 5] = [
        Self::Loading,
        Self::Converting,
        Self::Ready,
        Self::Unavailable,
        Self::Failed,
    ];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Converting => "converting",
            Self::Ready => "ready",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.token() == token)
    }
}

impl fmt::Display for DocumentState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.token())
    }
}

/// `text` without control characters (line breaks kept when `multiline`)
/// and cut to at most `maximum` bytes on a character boundary.
#[must_use]
pub fn bounded_paged_text(text: &str, maximum: usize, multiline: bool) -> String {
    let mut clean: String = text
        .chars()
        .filter(|c| !c.is_control() || (multiline && *c == '\n'))
        .collect();
    if clean.len() > maximum {
        let mut end = maximum;
        while !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean.truncate(end);
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_clamps_to_the_page_range() {
        assert_eq!(PagedNavigation::Next.target(0, 3), 1);
        assert_eq!(PagedNavigation::Next.target(2, 3), 2);
        assert_eq!(PagedNavigation::Previous.target(0, 3), 0);
        assert_eq!(PagedNavigation::Previous.target(2, 3), 1);
        assert_eq!(PagedNavigation::First.target(2, 3), 0);
        assert_eq!(PagedNavigation::Last.target(0, 3), 2);
        assert_eq!(PagedNavigation::Goto(9).target(0, 3), 2);
        assert_eq!(PagedNavigation::Goto(1).target(0, 3), 1);
        // A current page past a shrunk source counts from the last page.
        assert_eq!(PagedNavigation::Previous.target(7, 3), 1);
        assert_eq!(PagedNavigation::Next.target(7, 0), 0);
    }

    #[test]
    fn tokens_round_trip_and_goto_needs_a_page() {
        for step in [
            PagedNavigation::Next,
            PagedNavigation::Previous,
            PagedNavigation::First,
            PagedNavigation::Last,
        ] {
            assert_eq!(PagedNavigation::from_token(step.token(), None), Some(step));
            assert_eq!(PagedNavigation::from_token(step.token(), Some(1)), None);
        }
        assert_eq!(
            PagedNavigation::from_token("goto", Some(4)),
            Some(PagedNavigation::Goto(4))
        );
        assert_eq!(PagedNavigation::from_token("goto", None), None);
        assert_eq!(
            PagedNavigation::from_token("goto", Some(MAX_PAGED_PAGES)),
            None
        );
    }

    #[test]
    fn formats_come_from_extensions() {
        assert_eq!(
            DocumentFormat::from_file_name("deck.PDF"),
            Some(DocumentFormat::Pdf)
        );
        assert_eq!(
            DocumentFormat::from_file_name("asset://a/talk.pptx"),
            Some(DocumentFormat::Pptx)
        );
        assert_eq!(
            DocumentFormat::from_file_name("x.key"),
            Some(DocumentFormat::Keynote)
        );
        assert_eq!(DocumentFormat::from_file_name("clip.mp4"), None);
        assert!(DocumentFormat::Odp.needs_conversion());
        assert!(!DocumentFormat::Pdf.needs_conversion());
    }

    #[test]
    fn text_is_bounded_on_a_character_boundary() {
        assert_eq!(bounded_paged_text("a\u{7}b\nc", 10, true), "ab\nc");
        assert_eq!(bounded_paged_text("a\nb", 10, false), "ab");
        assert_eq!(bounded_paged_text("ñññ", 3, false), "ñ");
    }
}
