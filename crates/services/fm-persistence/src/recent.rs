//! A bounded, persisted most-recently-used list of project bundles.
//!
//! Entries are keyed by the canonical path of the `.freemix` directory, so
//! opening the same bundle through a symlink or a relative path moves the one
//! existing entry to the front instead of adding a duplicate. The list is
//! stored as a small JSON document using the exact
//! [`RECENT_PROJECTS_CONTRACT`]; on Unix a path that is not valid UTF-8 is
//! stored losslessly as hexadecimal bytes.

use std::{
    error::Error,
    fmt::{self, Write as _},
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    ProjectStore, fsutil,
    json::{Reader, SyntaxError, Value, escape_string},
};

/// Contract identifier written into every recent-projects document.
pub const RECENT_PROJECTS_CONTRACT: &str = "freemix.recent-projects";
/// Entries kept when the caller does not choose otherwise.
pub const DEFAULT_RECENT_PROJECTS: usize = 10;
/// Upper bound on the number of entries a list may hold.
pub const MAX_RECENT_PROJECTS: usize = 100;
/// Upper bound on a recent-projects document accepted by the parser.
pub const MAX_RECENT_PROJECTS_BYTES: u64 = 512 * 1024;
const MAX_RECENT_NAME_BYTES: usize = 256;

/// One remembered project bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentProject {
    /// Canonical path of the bundle directory when it was recorded.
    pub path: PathBuf,
    /// Display name captured when the project was opened.
    pub name: String,
    /// Caller-supplied open time in milliseconds since the Unix epoch.
    pub opened_at_unix_ms: u64,
}

/// Whether a remembered bundle is still openable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecentProjectStatus {
    /// The bundle directory and its `project.json` exist.
    Available,
    /// Nothing exists at the remembered path.
    Missing,
    /// Something exists there, but it is not a bundle with a manifest.
    NotABundle,
}

/// A failure to load, store, or record recent projects.
#[derive(Debug)]
pub enum RecentProjectsError {
    InvalidCapacity {
        requested: usize,
    },
    TooLarge {
        bytes: u64,
    },
    Syntax {
        offset: usize,
        message: String,
    },
    Shape(String),
    ContractMismatch,
    DuplicatePath(PathBuf),
    /// A path cannot be represented in the document on this platform.
    UnrepresentablePath(PathBuf),
    Io(io::Error),
}

impl fmt::Display for RecentProjectsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapacity { requested } => write!(
                formatter,
                "recent-projects capacity {requested} is outside 1..={MAX_RECENT_PROJECTS}"
            ),
            Self::TooLarge { bytes } => write!(
                formatter,
                "recent-projects document is {bytes} bytes, exceeding {MAX_RECENT_PROJECTS_BYTES}"
            ),
            Self::Syntax { offset, message } => write!(
                formatter,
                "malformed recent-projects document at byte {offset}: {message}"
            ),
            Self::Shape(message) => {
                write!(formatter, "invalid recent-projects document: {message}")
            }
            Self::ContractMismatch => write!(
                formatter,
                "recent-projects contract must be `{RECENT_PROJECTS_CONTRACT}`"
            ),
            Self::DuplicatePath(path) => {
                write!(
                    formatter,
                    "recent project `{}` is listed twice",
                    path.display()
                )
            }
            Self::UnrepresentablePath(path) => write!(
                formatter,
                "path `{}` cannot be stored on this platform",
                path.display()
            ),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl Error for RecentProjectsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for RecentProjectsError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Most-recently-opened bundles, newest first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentProjects {
    capacity: usize,
    entries: Vec<RecentProject>,
}

impl RecentProjects {
    /// Creates an empty list bounded to `capacity` entries.
    ///
    /// # Errors
    ///
    /// Returns [`RecentProjectsError::InvalidCapacity`] unless
    /// `1 <= capacity <= MAX_RECENT_PROJECTS`.
    pub fn new(capacity: usize) -> Result<Self, RecentProjectsError> {
        if !(1..=MAX_RECENT_PROJECTS).contains(&capacity) {
            return Err(RecentProjectsError::InvalidCapacity {
                requested: capacity,
            });
        }
        Ok(Self {
            capacity,
            entries: Vec::new(),
        })
    }

    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Entries, most recently opened first.
    #[must_use]
    pub fn entries(&self) -> &[RecentProject] {
        &self.entries
    }

    /// Records that `store`'s bundle was opened, moving an existing entry for
    /// the same canonical bundle to the front and evicting the oldest entry
    /// beyond capacity.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the bundle cannot be canonicalized (for
    /// example because it does not exist). The list is unchanged on error.
    pub fn record(
        &mut self,
        store: &ProjectStore,
        name: &str,
        opened_at_unix_ms: u64,
    ) -> Result<(), RecentProjectsError> {
        let path = fs::canonicalize(store.root())?;
        encode_path(&path)?;
        self.entries.retain(|entry| entry.path != path);
        self.entries.insert(
            0,
            RecentProject {
                path,
                name: bounded_name(name),
                opened_at_unix_ms,
            },
        );
        self.entries.truncate(self.capacity);
        Ok(())
    }

    /// Removes the entry for `path`, comparing canonically when the path still
    /// exists and literally otherwise. Returns whether an entry was removed.
    pub fn remove(&mut self, path: &Path) -> bool {
        let key = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let before = self.entries.len();
        self.entries.retain(|entry| entry.path != key);
        before != self.entries.len()
    }

    /// Reports each entry's availability, in list order.
    #[must_use]
    pub fn statuses(&self) -> Vec<(&RecentProject, RecentProjectStatus)> {
        self.entries
            .iter()
            .map(|entry| (entry, status_of(&entry.path)))
            .collect()
    }

    /// Removes and returns every entry that is no longer available.
    pub fn prune_unavailable(&mut self) -> Vec<RecentProject> {
        let (kept, removed) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|entry| status_of(&entry.path) == RecentProjectStatus::Available);
        self.entries = kept;
        removed
    }

    /// Loads a list from `path`, or returns an empty list when no file
    /// exists. Entries beyond `capacity` are dropped from the tail.
    ///
    /// # Errors
    ///
    /// Returns capacity, size, syntax, contract, shape, duplicate, or I/O
    /// errors. No partial list is returned.
    pub fn load(path: &Path, capacity: usize) -> Result<Self, RecentProjectsError> {
        let mut list = Self::new(capacity)?;
        let bytes = match fs::metadata(path) {
            Ok(metadata) if metadata.len() > MAX_RECENT_PROJECTS_BYTES => {
                return Err(RecentProjectsError::TooLarge {
                    bytes: metadata.len(),
                });
            }
            Ok(_) => fs::read(path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(list),
            Err(error) => return Err(error.into()),
        };
        let source = String::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        list.entries = parse(&source)?;
        list.entries.truncate(capacity);
        Ok(list)
    }

    /// Atomically replaces the document at `path` with this list.
    ///
    /// # Errors
    ///
    /// Returns an error when an entry's path cannot be represented or the
    /// file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), RecentProjectsError> {
        let document = self.encode()?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fsutil::write_atomically(path, document.as_bytes())?;
        Ok(())
    }

    fn encode(&self) -> Result<String, RecentProjectsError> {
        let mut output = String::from("{\"contract\":");
        push_string(&mut output, RECENT_PROJECTS_CONTRACT);
        output.push_str(",\"entries\":[");
        for (index, entry) in self.entries.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            output.push('{');
            match encode_path(&entry.path)? {
                EncodedPath::Text(text) => {
                    output.push_str("\"path\":");
                    push_string(&mut output, text);
                }
                EncodedPath::Hex(hex) => {
                    output.push_str("\"path_hex\":");
                    push_string(&mut output, &hex);
                }
            }
            output.push_str(",\"name\":");
            push_string(&mut output, &entry.name);
            let _ = write!(
                output,
                ",\"opened_at_unix_ms\":{}}}",
                entry.opened_at_unix_ms
            );
        }
        output.push_str("]}\n");
        Ok(output)
    }
}

fn status_of(path: &Path) -> RecentProjectStatus {
    match fs::metadata(path) {
        Err(_) => RecentProjectStatus::Missing,
        Ok(metadata)
            if metadata.is_dir()
                && ProjectStore::new(path.to_path_buf()).is_ok_and(|store| {
                    fs::metadata(store.manifest_path()).is_ok_and(|manifest| manifest.is_file())
                }) =>
        {
            RecentProjectStatus::Available
        }
        Ok(_) => RecentProjectStatus::NotABundle,
    }
}

fn bounded_name(name: &str) -> String {
    let mut end = name.len().min(MAX_RECENT_NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

fn push_string(output: &mut String, value: &str) {
    output.push('"');
    escape_string(output, value);
    output.push('"');
}

enum EncodedPath<'a> {
    Text(&'a str),
    // Only Unix paths can hold bytes that are not UTF-8.
    #[cfg_attr(not(unix), allow(dead_code))]
    Hex(String),
}

fn encode_path(path: &Path) -> Result<EncodedPath<'_>, RecentProjectsError> {
    if !path.is_absolute() {
        return Err(RecentProjectsError::UnrepresentablePath(path.to_path_buf()));
    }
    if let Some(text) = path.to_str() {
        return Ok(EncodedPath::Text(text));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let mut hex = String::with_capacity(path.as_os_str().len() * 2);
        for byte in path.as_os_str().as_bytes() {
            let _ = write!(hex, "{byte:02x}");
        }
        Ok(EncodedPath::Hex(hex))
    }
    #[cfg(not(unix))]
    {
        Err(RecentProjectsError::UnrepresentablePath(path.to_path_buf()))
    }
}

fn decode_hex_path(hex: &str) -> Result<PathBuf, RecentProjectsError> {
    let invalid = || RecentProjectsError::Shape("`path_hex` is not valid lowercase hex".into());
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return Err(invalid());
    }
    let nibble = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let digits = hex.as_bytes();
    let bytes = (0..digits.len())
        .step_by(2)
        .map(|index| Some((nibble(digits[index])? << 4) | nibble(digits[index + 1])?))
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(invalid)?;
    if std::str::from_utf8(&bytes).is_ok() {
        // Valid UTF-8 must use the `path` field, so each path has one form.
        return Err(RecentProjectsError::Shape(
            "`path_hex` holds a UTF-8 path; use `path`".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        let _ = bytes;
        Err(RecentProjectsError::Shape(
            "`path_hex` is only supported on Unix".into(),
        ))
    }
}

fn parse(source: &str) -> Result<Vec<RecentProject>, RecentProjectsError> {
    let shape = |message: &str| RecentProjectsError::Shape(message.to_owned());
    let document = Reader::new(source)
        .document()
        .map_err(
            |SyntaxError { offset, message }| RecentProjectsError::Syntax { offset, message },
        )?;
    let Value::Object(mut root) = document else {
        return Err(shape("document must be an object"));
    };
    let Some(Value::String(contract)) = root.remove("contract") else {
        return Err(shape("missing string field `contract`"));
    };
    if contract != RECENT_PROJECTS_CONTRACT {
        return Err(RecentProjectsError::ContractMismatch);
    }
    let Some(Value::Array(values)) = root.remove("entries") else {
        return Err(shape("missing array field `entries`"));
    };
    if let Some(field) = root.keys().next() {
        return Err(RecentProjectsError::Shape(format!(
            "unknown field `{field}`"
        )));
    }
    if values.len() > MAX_RECENT_PROJECTS {
        return Err(shape("too many entries"));
    }
    let mut entries: Vec<RecentProject> = Vec::with_capacity(values.len());
    for value in values {
        let Value::Object(mut object) = value else {
            return Err(shape("entry must be an object"));
        };
        let path = match (object.remove("path"), object.remove("path_hex")) {
            (Some(Value::String(text)), None) => PathBuf::from(text),
            (None, Some(Value::String(hex))) => decode_hex_path(&hex)?,
            _ => return Err(shape("entry needs exactly one string `path` or `path_hex`")),
        };
        if !path.is_absolute() {
            return Err(shape("entry path must be absolute"));
        }
        let Some(Value::String(name)) = object.remove("name") else {
            return Err(shape("entry is missing string field `name`"));
        };
        if name.len() > MAX_RECENT_NAME_BYTES {
            return Err(shape("entry name is too long"));
        }
        let Some(Value::Number(opened_at)) = object.remove("opened_at_unix_ms") else {
            return Err(shape("entry is missing integer field `opened_at_unix_ms`"));
        };
        let opened_at_unix_ms =
            u64::try_from(opened_at).map_err(|_| shape("`opened_at_unix_ms` is out of range"))?;
        if let Some(field) = object.keys().next() {
            return Err(RecentProjectsError::Shape(format!(
                "unknown entry field `{field}`"
            )));
        }
        if entries.iter().any(|entry| entry.path == path) {
            return Err(RecentProjectsError::DuplicatePath(path));
        }
        entries.push(RecentProject {
            path,
            name,
            opened_at_unix_ms,
        });
    }
    Ok(entries)
}
