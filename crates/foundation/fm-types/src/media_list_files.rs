//! The files a media list plays (IN-006) and M3U playlists, shared by the
//! model and the Studio client, which links no model crate.

use std::path::{Path, PathBuf};

/// Longest M3U text [`parse_m3u`] reads, in bytes.
pub const MAX_M3U_BYTES: usize = 1024 * 1024;

/// File extensions (lower case) of the video and audio files a media list
/// plays; still images are not list items.
pub const MEDIA_LIST_FILE_EXTENSIONS: [&str; 20] = [
    "mp4", "m4v", "mov", "mkv", "mxf", "ts", "m2ts", "mts", "webm", "avi", "nut", "mpg", "mpeg",
    "wav", "mp3", "aac", "m4a", "flac", "ogg", "opus",
];

/// Whether `path` (a host path or asset URI) names a video or audio file a
/// media list plays, by its extension (case-insensitive).
#[must_use]
pub fn is_media_list_file_name(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty()
            && MEDIA_LIST_FILE_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
    })
}

/// One entry of an M3U playlist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct M3uEntry {
    /// The file, resolved against the playlist's directory when relative.
    pub path: PathBuf,
    /// The `#EXTINF` title, when one precedes the entry.
    pub title: Option<String>,
    /// The `#EXTINF` duration in whole seconds, when positive.
    pub duration_seconds: Option<u64>,
}

/// Reads an M3U or M3U8 playlist.
///
/// Blank lines and comments are skipped; `#EXTINF:<seconds>,<title>`
/// titles the entry after it. Entries are file paths (absolute, or relative
/// to `base`, the playlist's directory) or `file://` URLs; other URLs
/// (`http://` and the like) are skipped, as are entries that are not video
/// or audio files ([`is_media_list_file_name`]). A leading byte-order mark
/// is ignored. At most `max_entries` entries are returned, and titles are
/// cut to `max_title_bytes`.
#[must_use]
pub fn parse_m3u(
    text: &str,
    base: &Path,
    max_entries: usize,
    max_title_bytes: usize,
) -> Vec<M3uEntry> {
    let text = text.get(..MAX_M3U_BYTES.min(text.len())).unwrap_or(text);
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut entries = Vec::new();
    let mut pending: Option<(Option<u64>, Option<String>)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            let (duration, title) = info.split_once(',').unwrap_or((info, ""));
            // Attributes (`-1 tvg-id="x"`) follow the duration.
            let duration = duration
                .split_whitespace()
                .next()
                .and_then(|seconds| seconds.parse::<f64>().ok())
                .filter(|seconds| seconds.is_finite() && *seconds >= 1.0)
                .map(|seconds| {
                    // Bounded: a whole number of seconds below 2^53.
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let whole = seconds.min(9.0e15).floor() as u64;
                    whole
                });
            let title: String = title.trim().chars().filter(|c| !c.is_control()).collect();
            let title = truncate(title.trim(), max_title_bytes);
            pending = Some((duration, (!title.is_empty()).then_some(title)));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let (duration_seconds, title) = pending.take().unwrap_or((None, None));
        let path = if let Some(rest) = line.strip_prefix("file://") {
            // `file:///abs/path` or `file://localhost/abs/path`.
            let rest = rest.strip_prefix("localhost").unwrap_or(rest);
            PathBuf::from(percent_decode(rest))
        } else if line.contains("://") {
            continue;
        } else {
            let candidate = PathBuf::from(line.replace('\\', std::path::MAIN_SEPARATOR_STR));
            if candidate.is_absolute() {
                candidate
            } else {
                base.join(candidate)
            }
        };
        if !path.to_str().is_some_and(is_media_list_file_name) {
            continue;
        }
        entries.push(M3uEntry {
            path,
            title,
            duration_seconds,
        });
        if entries.len() == max_entries {
            break;
        }
    }
    entries
}

fn truncate(text: &str, maximum: usize) -> String {
    if text.len() <= maximum {
        return text.to_owned();
    }
    let mut end = maximum;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = text.get(index + 1..index + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            decoded.push(byte);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(decoded).unwrap_or_else(|_| text.to_owned())
}

/// Whether `path` names an M3U playlist by its extension.
#[must_use]
pub fn is_m3u_file_name(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.eq_ignore_ascii_case("m3u") || extension.eq_ignore_ascii_case("m3u8")
    })
}
