//! Folder-listing input for slideshows.
//!
//! The caller supplies the directory's file names (this module never touches
//! the filesystem). Names are filtered to supported still-image extensions
//! (case-insensitive) and sorted with a deterministic natural order in which
//! digit runs compare numerically, so `img2.png` precedes `img10.png`.

use core::cmp::Ordering;
use core::fmt;
use std::collections::BTreeSet;

/// Maximum number of names accepted by [`ImageFolderListing::from_names`].
pub const MAX_FOLDER_ENTRIES: usize = 262_144;
/// Maximum length of one supplied name in bytes.
pub const MAX_FOLDER_NAME_BYTES: usize = 1_024;
/// Supported lowercase image extensions.
pub const IMAGE_EXTENSIONS: [&str; 8] = ["jpg", "jpeg", "png", "bmp", "gif", "tif", "tiff", "webp"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FolderListingError {
    TooManyEntries {
        count: usize,
        max: usize,
    },
    EmptyName {
        index: usize,
    },
    NameTooLong {
        index: usize,
        bytes: usize,
    },
    /// The name contains a path separator or NUL; only bare names are valid.
    InvalidName {
        index: usize,
    },
    DuplicateName(String),
}

impl fmt::Display for FolderListingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyEntries { count, max } => {
                write!(formatter, "folder has {count} entries; the limit is {max}")
            }
            Self::EmptyName { index } => write!(formatter, "folder entry {index} is empty"),
            Self::NameTooLong { index, bytes } => write!(
                formatter,
                "folder entry {index} is {bytes} bytes; the limit is {MAX_FOLDER_NAME_BYTES}"
            ),
            Self::InvalidName { index } => write!(
                formatter,
                "folder entry {index} contains a path separator or NUL"
            ),
            Self::DuplicateName(name) => write!(formatter, "folder entry {name:?} is duplicated"),
        }
    }
}

impl std::error::Error for FolderListingError {}

/// Filtered, naturally sorted image names from one folder.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImageFolderListing {
    images: Vec<String>,
    skipped: usize,
}

impl ImageFolderListing {
    /// Validates every name, keeps supported images, and sorts them.
    ///
    /// # Errors
    ///
    /// Returns an error for too many entries, an empty or oversized name, a
    /// name with a path separator or NUL, or an exact duplicate name.
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> Result<Self, FolderListingError> {
        if names.len() > MAX_FOLDER_ENTRIES {
            return Err(FolderListingError::TooManyEntries {
                count: names.len(),
                max: MAX_FOLDER_ENTRIES,
            });
        }
        let mut seen = BTreeSet::new();
        let mut images = Vec::new();
        let mut skipped = 0;
        for (index, name) in names.iter().enumerate() {
            let name = name.as_ref();
            if name.is_empty() {
                return Err(FolderListingError::EmptyName { index });
            }
            if name.len() > MAX_FOLDER_NAME_BYTES {
                return Err(FolderListingError::NameTooLong {
                    index,
                    bytes: name.len(),
                });
            }
            if name.contains(['/', '\\', '\0']) {
                return Err(FolderListingError::InvalidName { index });
            }
            if !seen.insert(name) {
                return Err(FolderListingError::DuplicateName(name.to_owned()));
            }
            if is_supported_image(name) {
                images.push(name.to_owned());
            } else {
                skipped += 1;
            }
        }
        images.sort_by(|a, b| natural_cmp(a, b));
        Ok(Self { images, skipped })
    }

    #[must_use]
    pub fn images(&self) -> &[String] {
        &self.images
    }

    #[must_use]
    pub fn into_images(self) -> Vec<String> {
        self.images
    }

    /// Number of valid names dropped by the extension filter.
    #[must_use]
    pub const fn skipped(&self) -> usize {
        self.skipped
    }
}

/// True when `name` has a supported image extension (case-insensitive).
#[must_use]
pub fn is_supported_image(name: &str) -> bool {
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && IMAGE_EXTENSIONS
            .iter()
            .any(|supported| extension.eq_ignore_ascii_case(supported))
}

/// Deterministic total natural order.
///
/// Names are split into digit and non-digit runs. Digit runs compare by
/// numeric value (leading zeros ignored, arbitrary length), non-digit runs by
/// ASCII-case-insensitive characters, and a digit run sorts before a
/// non-digit run. Ties are broken by fewer leading zeros, then by plain
/// byte order, so distinct names never compare equal.
#[must_use]
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut left = Runs::new(a);
    let mut right = Runs::new(b);
    let mut zero_tiebreak = Ordering::Equal;
    loop {
        match (left.next(), right.next()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let x_digit = x.as_bytes()[0].is_ascii_digit();
                let y_digit = y.as_bytes()[0].is_ascii_digit();
                let ordering = match (x_digit, y_digit) {
                    (true, true) => {
                        let xt = x.trim_start_matches('0');
                        let yt = y.trim_start_matches('0');
                        if zero_tiebreak == Ordering::Equal {
                            zero_tiebreak = x.len().cmp(&y.len());
                        }
                        xt.len().cmp(&yt.len()).then_with(|| xt.cmp(yt))
                    }
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => x
                        .chars()
                        .map(|c| c.to_ascii_lowercase())
                        .cmp(y.chars().map(|c| c.to_ascii_lowercase())),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
    zero_tiebreak.then_with(|| a.cmp(b))
}

struct Runs<'a> {
    rest: &'a str,
}

impl<'a> Runs<'a> {
    const fn new(text: &'a str) -> Self {
        Self { rest: text }
    }
}

impl<'a> Iterator for Runs<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let first = self.rest.as_bytes().first()?;
        let digit = first.is_ascii_digit();
        let end = self
            .rest
            .char_indices()
            .find(|(_, c)| c.is_ascii_digit() != digit)
            .map_or(self.rest.len(), |(index, _)| index);
        let (run, rest) = self.rest.split_at(end);
        self.rest = rest;
        Some(run)
    }
}
