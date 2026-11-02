//! On-disk browser profile isolation (IN-013).
//!
//! [`ProfileStore`] keeps every browser profile in its own directory under one
//! root, named by the profile's stable numeric id. A profile's cookies are
//! stored as the canonical `freemix-browser-cookies 1` document, written
//! atomically with owner-only permissions; loading is strict and applies the
//! same cookie limits as the in-memory jar.
//!
//! The store refuses a symlinked root or profile directory, so a profile can
//! never resolve outside the configured root. Opening a real browser profile
//! (and its engine data) remains an integration concern.

use core::fmt::{self, Write as _};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use crate::contract::{BrowserLimits, CookieError, CookieJar, ProfileId};

/// Cookie document header.
pub const PROFILE_COOKIE_DOCUMENT: &str = "freemix-browser-cookies 1";
/// Cookie file name inside one profile directory.
pub const PROFILE_COOKIE_FILE: &str = "cookies";
/// Largest cookie document accepted when loading.
pub const MAX_PROFILE_COOKIE_BYTES: usize = 8 * 1024 * 1024;

/// Why a profile cookie document could not be decoded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileDocumentError {
    /// The first line is not the current header.
    Header,
    /// A line is not `cookie <host> <name> <value>` in hex.
    Line,
    /// A hex field has an odd length or a non-hex character.
    Hex,
    /// A decoded field is not UTF-8.
    Utf8,
    /// The same host and name appear twice.
    Duplicate,
    /// The document is larger than [`MAX_PROFILE_COOKIE_BYTES`].
    TooLarge,
}

impl fmt::Display for ProfileDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header => formatter.write_str("browser profile document header is unknown"),
            Self::Line => formatter.write_str("browser profile cookie line is malformed"),
            Self::Hex => formatter.write_str("browser profile cookie field is not hex"),
            Self::Utf8 => formatter.write_str("browser profile cookie field is not UTF-8"),
            Self::Duplicate => formatter.write_str("browser profile cookie is duplicated"),
            Self::TooLarge => formatter.write_str("browser profile cookie document is too large"),
        }
    }
}

impl std::error::Error for ProfileDocumentError {}

/// Why a profile store operation failed.
#[derive(Debug)]
pub enum ProfileStoreError {
    Io(io::Error),
    Document(ProfileDocumentError),
    Cookie(CookieError),
}

impl fmt::Display for ProfileStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "browser profile store: {error}"),
            Self::Document(error) => write!(formatter, "browser profile document: {error}"),
            Self::Cookie(error) => write!(formatter, "browser profile cookie: {error}"),
        }
    }
}

impl std::error::Error for ProfileStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Document(error) => Some(error),
            Self::Cookie(error) => Some(error),
        }
    }
}

impl From<io::Error> for ProfileStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ProfileDocumentError> for ProfileStoreError {
    fn from(error: ProfileDocumentError) -> Self {
        Self::Document(error)
    }
}

impl From<CookieError> for ProfileStoreError {
    fn from(error: CookieError) -> Self {
        Self::Cookie(error)
    }
}

/// A root directory holding one isolated directory per browser profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileStore {
    root: PathBuf,
}

impl ProfileStore {
    /// Creates or opens `root`, requiring it to be a real directory.
    ///
    /// # Errors
    ///
    /// Returns the creation error, or [`io::ErrorKind::InvalidInput`] when the
    /// root is a symlink or not a directory.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        match fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "browser profile root must be a real directory",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut builder = DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt as _;
                    builder.mode(0o700);
                }
                builder.create(&root)?;
            }
            Err(error) => return Err(error),
        }
        Ok(Self { root })
    }

    /// The root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory for `profile_id`; its name is the numeric id, so it can
    /// never contain a path separator.
    #[must_use]
    pub fn path(&self, profile_id: ProfileId) -> PathBuf {
        self.root.join(profile_id.to_string())
    }

    /// Stores `jar`'s cookies for `profile_id` atomically.
    ///
    /// # Errors
    ///
    /// Returns an error for a symlinked profile directory, a failed write, or
    /// a cookie the jar's limits would refuse.
    pub fn save(&self, profile_id: ProfileId, jar: &CookieJar) -> Result<(), ProfileStoreError> {
        let directory = self.profile_directory(profile_id)?;
        let path = directory.join(PROFILE_COOKIE_FILE);
        let mut document = String::from(PROFILE_COOKIE_DOCUMENT);
        document.push('\n');
        for (host, name, value) in jar.profile(profile_id) {
            let _ = writeln!(
                document,
                "cookie {} {} {}",
                hex(host.as_bytes()),
                hex(name.as_bytes()),
                hex(value.as_bytes())
            );
        }
        if document.len() > MAX_PROFILE_COOKIE_BYTES {
            return Err(ProfileStoreError::Document(ProfileDocumentError::TooLarge));
        }
        let temporary = directory.join(format!(".{PROFILE_COOKIE_FILE}.tmp"));
        let mut file = {
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            options.open(&temporary)?
        };
        file.write_all(document.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &path)?;
        Ok(())
    }

    /// Loads `profile_id`'s cookies, or an empty jar when it has none.
    ///
    /// # Errors
    ///
    /// Returns an error for a symlinked profile directory, an unreadable file,
    /// a malformed document, or a cookie the supplied limits refuse.
    pub fn load(
        &self,
        profile_id: ProfileId,
        limits: BrowserLimits,
    ) -> Result<CookieJar, ProfileStoreError> {
        let directory = self.path(profile_id);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "browser profile directory must be a real directory",
                )
                .into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(CookieJar::new());
            }
            Err(error) => return Err(error.into()),
        }
        let path = directory.join(PROFILE_COOKIE_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(CookieJar::new()),
            Err(error) => return Err(error.into()),
        };
        if text.len() > MAX_PROFILE_COOKIE_BYTES {
            return Err(ProfileDocumentError::TooLarge.into());
        }
        let mut jar = CookieJar::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut lines = text.lines();
        if lines.next() != Some(PROFILE_COOKIE_DOCUMENT) {
            return Err(ProfileDocumentError::Header.into());
        }
        for line in lines {
            let mut fields = line.split(' ');
            if fields.next() != Some("cookie") {
                return Err(ProfileDocumentError::Line.into());
            }
            let host = decode(fields.next().ok_or(ProfileDocumentError::Line)?)?;
            let name = decode(fields.next().ok_or(ProfileDocumentError::Line)?)?;
            let value = decode(fields.next().ok_or(ProfileDocumentError::Line)?)?;
            if fields.next().is_some() {
                return Err(ProfileDocumentError::Line.into());
            }
            if !seen.insert((host.clone(), name.clone())) {
                return Err(ProfileDocumentError::Duplicate.into());
            }
            jar.set(profile_id, &host, &name, &value, limits)?;
        }
        Ok(jar)
    }

    /// Removes one profile's cookie file and its directory when empty.
    ///
    /// # Errors
    ///
    /// Refuses a symlinked profile directory and returns the removal error.
    pub fn clear(&self, profile_id: ProfileId) -> Result<(), ProfileStoreError> {
        let directory = self.path(profile_id);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "browser profile directory must be a real directory",
                )
                .into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        match fs::remove_file(directory.join(PROFILE_COOKIE_FILE)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let _ = fs::remove_dir(&directory);
        Ok(())
    }

    fn profile_directory(&self, profile_id: ProfileId) -> Result<PathBuf, ProfileStoreError> {
        let directory = self.path(profile_id);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "browser profile directory must be a real directory",
                )
                .into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut builder = DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt as _;
                    builder.mode(0o700);
                }
                builder.create(&directory)?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(directory)
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn decode(text: &str) -> Result<String, ProfileDocumentError> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ProfileDocumentError::Hex);
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks_exact(2) {
        let byte = u8::from_str_radix(
            core::str::from_utf8(pair).map_err(|_| ProfileDocumentError::Hex)?,
            16,
        )
        .map_err(|_| ProfileDocumentError::Hex)?;
        bytes.push(byte);
    }
    String::from_utf8(bytes).map_err(|_| ProfileDocumentError::Utf8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        num::NonZeroU128,
        sync::atomic::{AtomicU64, Ordering},
    };

    fn temporary_directory() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-browser-profile-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn profile(value: u128) -> ProfileId {
        ProfileId::new(NonZeroU128::new(value).unwrap())
    }

    fn jar(cookies: &[(u128, &str, &str, &str)]) -> CookieJar {
        let mut jar = CookieJar::new();
        for (id, host, name, value) in cookies {
            jar.set(profile(*id), host, name, value, BrowserLimits::default())
                .unwrap();
        }
        jar
    }

    #[test]
    fn cookies_round_trip_per_profile_and_are_isolated() {
        let root = temporary_directory();
        let store = ProfileStore::open(&root).unwrap();
        let jar = jar(&[
            (1, "example.com", "session", "a b\tc"),
            (1, "example.com", "theme", "dark"),
            (2, "example.net", "session", "other"),
        ]);
        store.save(profile(1), &jar).unwrap();
        store.save(profile(2), &jar).unwrap();

        let first = store.load(profile(1), BrowserLimits::default()).unwrap();
        assert_eq!(
            first.profile(profile(1)).collect::<Vec<_>>(),
            vec![
                ("example.com", "session", "a b\tc"),
                ("example.com", "theme", "dark")
            ]
        );
        assert_eq!(first.profile(profile(2)).count(), 0);

        let second = store.load(profile(2), BrowserLimits::default()).unwrap();
        assert_eq!(
            second.profile(profile(2)).collect::<Vec<_>>(),
            vec![("example.net", "session", "other")]
        );
        assert_eq!(second.profile(profile(1)).count(), 0);
        assert!(store.path(profile(1)).starts_with(store.root()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let directory = fs::metadata(store.path(profile(1))).unwrap();
            assert_eq!(directory.permissions().mode() & 0o777, 0o700);
            let file = fs::metadata(store.path(profile(1)).join(PROFILE_COOKIE_FILE)).unwrap();
            assert_eq!(file.permissions().mode() & 0o777, 0o600);
        }

        store.clear(profile(1)).unwrap();
        assert_eq!(
            store
                .load(profile(1), BrowserLimits::default())
                .unwrap()
                .profile(profile(1))
                .count(),
            0
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_file_is_empty_and_a_malformed_document_is_typed() {
        let root = temporary_directory();
        let store = ProfileStore::open(&root).unwrap();
        assert_eq!(
            store
                .load(profile(3), BrowserLimits::default())
                .unwrap()
                .profile(profile(3))
                .count(),
            0
        );

        let directory = store.path(profile(3));
        fs::create_dir_all(&directory).unwrap();
        let document = directory.join(PROFILE_COOKIE_FILE);
        for text in [
            "not a profile\n",
            "freemix-browser-cookies 1\ncookie zz zz zz\n",
            "freemix-browser-cookies 1\ncookie 61 62\n",
            "freemix-browser-cookies 1\ncookie 61 62 ff\n",
            "freemix-browser-cookies 1\ncookie 6578616d706c652e636f6d 73657373696f6e 6f6e65\ncookie 6578616d706c652e636f6d 73657373696f6e 74776f\n",
        ] {
            fs::write(&document, text).unwrap();
            assert!(
                store.load(profile(3), BrowserLimits::default()).is_err(),
                "accepted {text:?}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_profile_or_root_is_refused() {
        use std::os::unix::fs::symlink;

        let root = temporary_directory();
        let outside = temporary_directory();
        let store = ProfileStore::open(&root).unwrap();
        let directory = store.path(profile(4));
        symlink(&outside, &directory).unwrap();
        assert!(store.save(profile(4), &jar(&[])).is_err());
        assert!(store.load(profile(4), BrowserLimits::default()).is_err());
        assert!(store.clear(profile(4)).is_err());

        let linked_root = root.join("linked");
        symlink(&outside, &linked_root).unwrap();
        assert!(ProfileStore::open(&linked_root).is_err());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
