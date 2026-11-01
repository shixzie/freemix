//! Bounded M3U / M3U8 playlist parsing for Media List import (IN-006).
//!
//! Only the local-file subset is accepted: one path per line, with an
//! optional `#EXTINF:<duration>,<title>` line immediately before it. Comments
//! and blank lines are ignored, and a URL entry is refused because a Media
//! List item is a bundle `asset://` file, not a network source.

use core::fmt;

/// Largest accepted playlist document, in bytes.
pub const MAX_M3U_BYTES: usize = 1024 * 1024;
/// Most entries one playlist may name.
pub const MAX_M3U_ENTRIES: usize = 256;
/// Longest accepted entry path, in bytes.
pub const MAX_M3U_PATH_BYTES: usize = 4096;
/// Longest accepted `#EXTINF` title, in bytes.
pub const MAX_M3U_TITLE_BYTES: usize = 128;

/// One playlist line that names a local media file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct M3uEntry {
    /// The path exactly as the playlist wrote it, before resolving it
    /// against the playlist's directory.
    pub path: String,
    /// The `#EXTINF` title, when one was given.
    pub title: Option<String>,
}

/// Why a playlist could not be read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum M3uError {
    /// The document is larger than [`MAX_M3U_BYTES`].
    TooLarge { bytes: usize },
    /// More than [`MAX_M3U_ENTRIES`] file entries.
    TooManyEntries { count: usize },
    /// The document is not UTF-8.
    InvalidUtf8,
    /// The playlist names no file.
    Empty,
    /// A path line is blank.
    BlankPath { line: usize },
    /// A path line is longer than [`MAX_M3U_PATH_BYTES`].
    PathTooLong { line: usize, bytes: usize },
    /// An `#EXTINF` title is longer than [`MAX_M3U_TITLE_BYTES`].
    TitleTooLong { line: usize, bytes: usize },
    /// A path or title contains a control character.
    ControlCharacter { line: usize },
    /// An entry is a URL, not a local path.
    UrlUnsupported { line: usize },
    /// The document ends with an `#EXTINF` that names no file.
    MissingPath { line: usize },
}

impl fmt::Display for M3uError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes } => write!(
                formatter,
                "playlist is {bytes} bytes, maximum is {MAX_M3U_BYTES}"
            ),
            Self::TooManyEntries { count } => write!(
                formatter,
                "playlist names {count} files, maximum is {MAX_M3U_ENTRIES}"
            ),
            Self::InvalidUtf8 => formatter.write_str("playlist is not UTF-8"),
            Self::Empty => formatter.write_str("playlist names no files"),
            Self::BlankPath { line } => write!(formatter, "playlist line {line} is blank"),
            Self::PathTooLong { line, bytes } => write!(
                formatter,
                "playlist line {line} is {bytes} bytes, maximum is {MAX_M3U_PATH_BYTES}"
            ),
            Self::TitleTooLong { line, bytes } => write!(
                formatter,
                "playlist title on line {line} is {bytes} bytes, maximum is {MAX_M3U_TITLE_BYTES}"
            ),
            Self::ControlCharacter { line } => {
                write!(
                    formatter,
                    "playlist line {line} contains a control character"
                )
            }
            Self::UrlUnsupported { line } => write!(
                formatter,
                "playlist line {line} names a URL; a Media List imports local files"
            ),
            Self::MissingPath { line } => {
                write!(formatter, "playlist title on line {line} names no file")
            }
        }
    }
}

impl std::error::Error for M3uError {}

/// Parses an M3U or M3U8 document into its local-file entries.
///
/// # Errors
///
/// Returns the first [`M3uError`] found.
pub fn parse_m3u(bytes: &[u8]) -> Result<Vec<M3uEntry>, M3uError> {
    if bytes.len() > MAX_M3U_BYTES {
        return Err(M3uError::TooLarge { bytes: bytes.len() });
    }
    let text = core::str::from_utf8(bytes).map_err(|_| M3uError::InvalidUtf8)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut entries = Vec::new();
    let mut title: Option<String> = None;
    let mut title_line: Option<usize> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("#EXTINF:") {
            let (_, name) = rest.split_once(',').unwrap_or((rest, ""));
            let name = name.trim();
            if name.len() > MAX_M3U_TITLE_BYTES {
                return Err(M3uError::TitleTooLong {
                    line,
                    bytes: name.len(),
                });
            }
            if name.chars().any(char::is_control) {
                return Err(M3uError::ControlCharacter { line });
            }
            title = (!name.is_empty()).then(|| name.to_owned());
            title_line = Some(line);
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        if entries.len() >= MAX_M3U_ENTRIES {
            return Err(M3uError::TooManyEntries {
                count: entries.len() + 1,
            });
        }
        if trimmed.len() > MAX_M3U_PATH_BYTES {
            return Err(M3uError::PathTooLong {
                line,
                bytes: trimmed.len(),
            });
        }
        if trimmed.chars().any(char::is_control) {
            return Err(M3uError::ControlCharacter { line });
        }
        if trimmed.contains("://") {
            return Err(M3uError::UrlUnsupported { line });
        }
        entries.push(M3uEntry {
            path: trimmed.to_owned(),
            title: title.take(),
        });
        title_line = None;
    }
    if let Some(line) = title_line {
        return Err(M3uError::MissingPath { line });
    }
    if entries.is_empty() {
        return Err(M3uError::Empty);
    }
    Ok(entries)
}
