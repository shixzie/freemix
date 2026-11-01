//! Installed plugin discovery (AU-006).
//!
//! A plugin root holds one directory per plugin, each with a `plugin.ini`
//! manifest of `key=value` lines:
//!
//! ```text
//! id=com.example.eq
//! name=Example EQ
//! api_major=1
//! api_minor=0
//! state_version=1
//! capability=audio.process
//! ```
//!
//! `id` is required and uses `[A-Za-z0-9._-]`; `name` defaults to the id;
//! the versions default to `0`, `0`, and `0`; `capability` may repeat up to
//! [`MAX_CAPABILITIES`]. A malformed manifest is skipped and reported by
//! [`PluginCatalog::skipped`], never failing the whole scan: one broken
//! plugin directory must not hide the others.
//!
//! Scanning lists what is installed; it does not load or run a plugin. A
//! chain naming an installed plugin is still refused by a host without a
//! processor for it.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::{ApiVersion, CapabilityId, PluginId, PluginManifest, StateVersion};

/// The manifest file inside every plugin directory.
pub const MANIFEST_FILE: &str = "plugin.ini";
/// Largest manifest read, in bytes.
pub const MAX_MANIFEST_BYTES: usize = 4 * 1024;
/// Most plugin directories one catalog holds.
pub const MAX_INSTALLED_PLUGINS: usize = 64;
/// Most capabilities one manifest requests.
pub const MAX_CAPABILITIES: usize = 32;
/// Longest plugin or capability identifier, in bytes.
pub const MAX_IDENTIFIER_BYTES: usize = 128;
/// Longest plugin name, in bytes.
pub const MAX_NAME_BYTES: usize = 128;

/// One installed plugin's manifest and display name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledPlugin {
    /// The plugin identity and versions.
    pub manifest: PluginManifest,
    /// The human-readable name (the id when the manifest names none).
    pub name: String,
}

impl InstalledPlugin {
    /// The plugin's identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        self.manifest.id.as_str()
    }
}

/// Why one plugin directory was skipped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkippedPlugin {
    /// The directory, as a display string (never an absolute path in a
    /// wire record).
    pub directory: String,
    /// What was wrong.
    pub reason: CatalogError,
}

/// The installed plugins under one root, sorted by id.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginCatalog {
    plugins: Vec<InstalledPlugin>,
    skipped: Vec<SkippedPlugin>,
    /// Why the root could not be listed, if it could not.
    error: Option<CatalogError>,
}

impl PluginCatalog {
    /// Scans `root`. A missing root is an empty catalog; an unreadable one
    /// carries [`CatalogError::Root`].
    #[must_use]
    pub fn scan(root: &Path) -> Self {
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self::default();
            }
            Err(error) => {
                return Self {
                    error: Some(CatalogError::Root(error.to_string())),
                    ..Self::default()
                };
            }
        };
        let mut plugins = Vec::new();
        let mut skipped = Vec::new();
        let mut seen = BTreeSet::new();
        let mut directories: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect();
        directories.sort();
        for directory in directories {
            let shown = directory.file_name().map_or_else(
                || directory.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            if plugins.len() >= MAX_INSTALLED_PLUGINS {
                skipped.push(SkippedPlugin {
                    directory: shown,
                    reason: CatalogError::TooManyPlugins,
                });
                continue;
            }
            match read_manifest(&directory) {
                Ok(plugin) => {
                    if seen.insert(plugin.id().to_owned()) {
                        plugins.push(plugin);
                    } else {
                        skipped.push(SkippedPlugin {
                            directory: shown,
                            reason: CatalogError::DuplicateId(plugin.id().to_owned()),
                        });
                    }
                }
                Err(reason) => skipped.push(SkippedPlugin {
                    directory: shown,
                    reason,
                }),
            }
        }
        plugins.sort_by(|left, right| left.id().cmp(right.id()));
        Self {
            plugins,
            skipped,
            error: None,
        }
    }

    /// The installed plugins, sorted by id.
    #[must_use]
    pub fn plugins(&self) -> &[InstalledPlugin] {
        &self.plugins
    }

    /// Every installed plugin identifier, sorted.
    #[must_use]
    pub fn identifiers(&self) -> Vec<String> {
        self.plugins
            .iter()
            .map(|plugin| plugin.id().to_owned())
            .collect()
    }

    /// The plugin with `id`, if installed.
    #[must_use]
    pub fn plugin(&self, id: &str) -> Option<&InstalledPlugin> {
        self.plugins.iter().find(|plugin| plugin.id() == id)
    }

    /// The directories skipped and why, sorted by directory.
    #[must_use]
    pub fn skipped(&self) -> &[SkippedPlugin] {
        &self.skipped
    }

    /// Why the root could not be listed, if it could not.
    #[must_use]
    pub fn error(&self) -> Option<&CatalogError> {
        self.error.as_ref()
    }
}

/// A manifest or root that cannot be used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogError {
    /// The root directory could not be listed.
    Root(String),
    /// The manifest file is missing or unreadable.
    Manifest(String),
    /// The manifest is larger than [`MAX_MANIFEST_BYTES`].
    TooLarge,
    /// A line is not `key=value`.
    Syntax(String),
    /// A field appears twice.
    DuplicateField(String),
    /// A required field is absent.
    MissingField(String),
    /// A field value is outside its contract.
    InvalidField(String),
    /// More capabilities than [`MAX_CAPABILITIES`].
    TooManyCapabilities,
    /// Another plugin already uses the id.
    DuplicateId(String),
    /// The catalog already holds [`MAX_INSTALLED_PLUGINS`].
    TooManyPlugins,
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root(reason) => write!(formatter, "plugin root: {reason}"),
            Self::Manifest(reason) => write!(formatter, "plugin manifest: {reason}"),
            Self::TooLarge => formatter.write_str("plugin manifest is too large"),
            Self::Syntax(line) => write!(formatter, "plugin manifest line: {line}"),
            Self::DuplicateField(field) => write!(formatter, "plugin manifest repeats {field}"),
            Self::MissingField(field) => write!(formatter, "plugin manifest lacks {field}"),
            Self::InvalidField(field) => write!(formatter, "plugin manifest field {field}"),
            Self::TooManyCapabilities => {
                formatter.write_str("plugin requests too many capabilities")
            }
            Self::DuplicateId(id) => write!(formatter, "plugin {id} is installed twice"),
            Self::TooManyPlugins => formatter.write_str("too many installed plugins"),
        }
    }
}

impl std::error::Error for CatalogError {}

fn read_manifest(directory: &Path) -> Result<InstalledPlugin, CatalogError> {
    let path = directory.join(MANIFEST_FILE);
    let metadata =
        std::fs::metadata(&path).map_err(|error| CatalogError::Manifest(error.to_string()))?;
    if metadata.len() > MAX_MANIFEST_BYTES as u64 {
        return Err(CatalogError::TooLarge);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|error| CatalogError::Manifest(error.to_string()))?;
    parse_manifest(&text)
}

/// Parses one `plugin.ini` manifest.
///
/// # Errors
///
/// Returns the first syntax, duplicate, or out-of-contract field.
pub fn parse_manifest(text: &str) -> Result<InstalledPlugin, CatalogError> {
    let mut id = None;
    let mut name = None;
    let mut api_major = None;
    let mut api_minor = None;
    let mut state_version = None;
    let mut capabilities = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(CatalogError::Syntax(line.to_owned()));
        };
        let key = key.trim();
        let value = value.trim();
        let slot = |field: &'static str| -> Result<(), CatalogError> {
            Err(CatalogError::DuplicateField(field.to_owned()))
        };
        match key {
            "id" => {
                if id.is_some() {
                    slot("id")?;
                }
                id = Some(value.to_owned());
            }
            "name" => {
                if name.is_some() {
                    slot("name")?;
                }
                name = Some(value.to_owned());
            }
            "api_major" => {
                if api_major.is_some() {
                    slot("api_major")?;
                }
                api_major = Some(parse_u16("api_major", value)?);
            }
            "api_minor" => {
                if api_minor.is_some() {
                    slot("api_minor")?;
                }
                api_minor = Some(parse_u16("api_minor", value)?);
            }
            "state_version" => {
                if state_version.is_some() {
                    slot("state_version")?;
                }
                state_version = Some(parse_u32("state_version", value)?);
            }
            "capability" => {
                if capabilities.len() >= MAX_CAPABILITIES {
                    return Err(CatalogError::TooManyCapabilities);
                }
                if !identifier_is_valid(value) {
                    return Err(CatalogError::InvalidField("capability".to_owned()));
                }
                capabilities.push(CapabilityId::new(value));
            }
            other => return Err(CatalogError::InvalidField(other.to_owned())),
        }
    }
    let Some(id) = id else {
        return Err(CatalogError::MissingField("id".to_owned()));
    };
    if !identifier_is_valid(&id) {
        return Err(CatalogError::InvalidField("id".to_owned()));
    }
    let name = name.unwrap_or_else(|| id.clone());
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(CatalogError::InvalidField("name".to_owned()));
    }
    let mut manifest = PluginManifest::new(
        PluginId::new(id),
        ApiVersion::new(api_major.unwrap_or(0), api_minor.unwrap_or(0)),
        StateVersion::new(state_version.unwrap_or(0)),
    );
    manifest.requested_capabilities = capabilities;
    Ok(InstalledPlugin { manifest, name })
}

fn identifier_is_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn parse_u16(field: &'static str, value: &str) -> Result<u16, CatalogError> {
    value
        .parse()
        .map_err(|_| CatalogError::InvalidField(field.to_owned()))
}

fn parse_u32(field: &'static str, value: &str) -> Result<u32, CatalogError> {
    value
        .parse()
        .map_err(|_| CatalogError::InvalidField(field.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_parse_their_fields_and_defaults() {
        let plugin = parse_manifest(
            "id=com.example.eq\nname=Example EQ\napi_major=2\napi_minor=1\n\
             state_version=7\ncapability=audio.process\ncapability=ui.panel\n",
        )
        .unwrap();
        assert_eq!(plugin.id(), "com.example.eq");
        assert_eq!(plugin.name, "Example EQ");
        assert_eq!(plugin.manifest.api_version, ApiVersion::new(2, 1));
        assert_eq!(plugin.manifest.state_version, StateVersion::new(7));
        assert_eq!(
            plugin
                .manifest
                .requested_capabilities
                .iter()
                .map(CapabilityId::as_str)
                .collect::<Vec<_>>(),
            ["audio.process", "ui.panel"]
        );

        let minimal = parse_manifest("id=plain\n").unwrap();
        assert_eq!(minimal.name, "plain");
        assert_eq!(minimal.manifest.api_version, ApiVersion::new(0, 0));
    }

    #[test]
    fn malformed_manifests_are_refused() {
        for (text, error) in [
            ("name=No id\n", CatalogError::MissingField("id".to_owned())),
            (
                "id=two words\n",
                CatalogError::InvalidField("id".to_owned()),
            ),
            (
                "id=a\nid=b\n",
                CatalogError::DuplicateField("id".to_owned()),
            ),
            (
                "id=a\napi_major=x\n",
                CatalogError::InvalidField("api_major".to_owned()),
            ),
            (
                "id=a\nunknown=1\n",
                CatalogError::InvalidField("unknown".to_owned()),
            ),
            ("id=a\nbroken\n", CatalogError::Syntax("broken".to_owned())),
        ] {
            assert_eq!(parse_manifest(text), Err(error), "{text:?}");
        }
    }
}

#[cfg(test)]
mod scan_tests {
    use super::*;
    use std::fs;

    fn directory(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("fm-plugin-catalog-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn scanning_lists_valid_manifests_and_reports_the_others() {
        let root = directory("mixed");
        let first = root.join("first");
        fs::create_dir(&first).unwrap();
        fs::write(
            first.join(MANIFEST_FILE),
            "id=com.example.eq\nname=Example EQ\napi_major=1\napi_minor=2\n",
        )
        .unwrap();
        let second = root.join("second");
        fs::create_dir(&second).unwrap();
        fs::write(second.join(MANIFEST_FILE), "id=com.example.clip\n").unwrap();
        let broken = root.join("broken");
        fs::create_dir(&broken).unwrap();
        fs::write(broken.join(MANIFEST_FILE), "id=two words\n").unwrap();
        let missing = root.join("missing");
        fs::create_dir(&missing).unwrap();
        let duplicate = root.join("zz-duplicate");
        fs::create_dir(&duplicate).unwrap();
        fs::write(duplicate.join(MANIFEST_FILE), "id=com.example.eq\n").unwrap();

        let catalog = PluginCatalog::scan(&root);
        assert_eq!(catalog.error(), None);
        assert_eq!(
            catalog.identifiers(),
            ["com.example.clip", "com.example.eq"]
        );
        let eq = catalog.plugin("com.example.eq").unwrap();
        assert_eq!(eq.name, "Example EQ");
        assert_eq!(eq.manifest.api_version, ApiVersion::new(1, 2));
        assert_eq!(catalog.skipped().len(), 3);
        assert_eq!(
            catalog.plugin("com.example.eq").map(InstalledPlugin::id),
            Some("com.example.eq")
        );
        assert_eq!(catalog.plugin("com.example.missing"), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_root_is_an_empty_catalog() {
        let root = std::env::temp_dir().join(format!(
            "fm-plugin-catalog-{}-missing-root",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let catalog = PluginCatalog::scan(&root);
        assert!(catalog.plugins().is_empty());
        assert!(catalog.skipped().is_empty());
        assert_eq!(catalog.error(), None);
    }
}
