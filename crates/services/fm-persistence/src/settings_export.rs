//! Secret-free settings export documents.
//!
//! A [`SettingsSnapshot`] is a flat, deterministic map from stable key paths
//! (`outputs.7.name`, `stream_targets.3.key`, ...) to scalar values. Fields
//! whose final segment names a credential (see [`SECRET_FIELD_NAMES`]) must
//! hold a [`SettingValue::Secret`], and every other field must not, so a
//! credential cannot be smuggled through a text value. The export encoder
//! writes every secret as a `{"secret_ref": "<key path>"}` placeholder and
//! never reads secret text, so an export document excludes secrets by
//! construction. Parsing an export therefore always yields
//! [`SecretSlot::Placeholder`] for secret fields.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Write as _},
    num::NonZeroU128,
};

use fm_model::{Project, RestartPolicy, StartupPolicy, StreamProtocol};

use crate::json::{DecodeError, Reader, Value, escape_string};

/// The exact `contract` tag every settings export document carries.
pub const SETTINGS_EXPORT_CONTRACT: &str = "freemix.settings-export";
/// Upper bound on an export document accepted by the parser.
pub const MAX_SETTINGS_EXPORT_BYTES: usize = 1024 * 1024;
/// Upper bound on entries in one snapshot or export document.
pub const MAX_SETTINGS_ENTRIES: usize = 4_096;
/// Upper bound on one key path, in bytes.
pub const MAX_SETTING_KEY_BYTES: usize = 256;
/// Upper bound on one key path segment, in bytes.
pub const MAX_SETTING_KEY_SEGMENT_BYTES: usize = 64;
/// Upper bound on key path segment count.
pub const MAX_SETTING_KEY_SEGMENTS: usize = 4;
/// Upper bound on one text value, in bytes.
pub const MAX_SETTING_TEXT_BYTES: usize = 4_096;
/// Upper bound on one secret, in bytes.
pub const MAX_SECRET_BYTES: usize = 4_096;
/// Final key segments that always denote a credential.
pub const SECRET_FIELD_NAMES: [&str; 3] = ["key", "token", "password"];

/// One top-level settings section.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SettingsSection {
    Format,
    RestartPolicy,
    Outputs,
    AudioBuses,
    StreamTargets,
}

impl SettingsSection {
    pub const ALL: [Self; 5] = [
        Self::Format,
        Self::RestartPolicy,
        Self::Outputs,
        Self::AudioBuses,
        Self::StreamTargets,
    ];

    /// The key path segment naming this section.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::RestartPolicy => "restart_policy",
            Self::Outputs => "outputs",
            Self::AudioBuses => "audio_buses",
            Self::StreamTargets => "stream_targets",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|section| section.name() == name)
    }

    /// Whether the section holds id-keyed entities (`<section>.<id>.<field>`)
    /// rather than one singleton entity.
    #[must_use]
    pub const fn is_collection(self) -> bool {
        matches!(self, Self::Outputs | Self::AudioBuses | Self::StreamTargets)
    }

    /// The reference kind an entity of this section defines, if any.
    #[must_use]
    pub const fn defines(self) -> Option<ReferenceKind> {
        match self {
            Self::Outputs => Some(ReferenceKind::Output),
            Self::AudioBuses => Some(ReferenceKind::AudioBus),
            Self::Format | Self::RestartPolicy | Self::StreamTargets => None,
        }
    }
}

impl fmt::Display for SettingsSection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The unit merge strategies choose between: a singleton section or one
/// id-keyed entity of a collection section.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntityKey {
    section: SettingsSection,
    id: Option<NonZeroU128>,
}

impl EntityKey {
    /// Builds an entity key; `id` must be `Some` exactly for collections.
    #[must_use]
    pub const fn new(section: SettingsSection, id: Option<NonZeroU128>) -> Option<Self> {
        if section.is_collection() == id.is_some() {
            Some(Self { section, id })
        } else {
            None
        }
    }

    #[must_use]
    pub const fn section(self) -> SettingsSection {
        self.section
    }

    #[must_use]
    pub const fn id(self) -> Option<NonZeroU128> {
        self.id
    }
}

impl fmt::Display for EntityKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.id {
            Some(id) => write!(formatter, "{}.{id}", self.section.name()),
            None => formatter.write_str(self.section.name()),
        }
    }
}

/// Why a key path was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingKeyError {
    TooLong,
    TooFewSegments,
    TooManySegments,
    EmptySegment,
    SegmentTooLong,
    /// Segments are restricted to `[a-z0-9_]`.
    InvalidCharacter,
    UnknownSection,
    /// The id segment of a collection key is not a canonical nonzero integer.
    InvalidEntityId,
}

impl fmt::Display for SettingKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLong => "setting key is too long",
            Self::TooFewSegments => "setting key has too few segments",
            Self::TooManySegments => "setting key has too many segments",
            Self::EmptySegment => "setting key has an empty segment",
            Self::SegmentTooLong => "setting key segment is too long",
            Self::InvalidCharacter => "setting key segments must be [a-z0-9_]",
            Self::UnknownSection => "setting key names an unknown section",
            Self::InvalidEntityId => "setting key entity id must be a canonical nonzero integer",
        })
    }
}

impl std::error::Error for SettingKeyError {}

/// One validated, stable key path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SettingKey {
    text: String,
    entity: EntityKey,
}

impl SettingKey {
    /// Validates a dotted key path.
    ///
    /// # Errors
    ///
    /// Returns [`SettingKeyError`] when the path is oversized, malformed,
    /// names an unknown section, or carries a non-canonical entity id.
    pub fn parse(text: &str) -> Result<Self, SettingKeyError> {
        if text.len() > MAX_SETTING_KEY_BYTES {
            return Err(SettingKeyError::TooLong);
        }
        let segments: Vec<&str> = text.split('.').collect();
        if segments.len() > MAX_SETTING_KEY_SEGMENTS {
            return Err(SettingKeyError::TooManySegments);
        }
        for segment in &segments {
            if segment.is_empty() {
                return Err(SettingKeyError::EmptySegment);
            }
            if segment.len() > MAX_SETTING_KEY_SEGMENT_BYTES {
                return Err(SettingKeyError::SegmentTooLong);
            }
            if !segment
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            {
                return Err(SettingKeyError::InvalidCharacter);
            }
        }
        if segments.len() < 2 {
            return Err(SettingKeyError::TooFewSegments);
        }
        let section =
            SettingsSection::from_name(segments[0]).ok_or(SettingKeyError::UnknownSection)?;
        let id = if section.is_collection() {
            if segments.len() < 3 {
                return Err(SettingKeyError::TooFewSegments);
            }
            Some(parse_canonical_id(segments[1]).ok_or(SettingKeyError::InvalidEntityId)?)
        } else {
            None
        };
        Ok(Self {
            text: text.to_owned(),
            entity: EntityKey { section, id },
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn entity(&self) -> EntityKey {
        self.entity
    }

    #[must_use]
    pub const fn section(&self) -> SettingsSection {
        self.entity.section
    }

    /// The final key path segment.
    #[must_use]
    pub fn field(&self) -> &str {
        self.text.rsplit('.').next().unwrap_or(&self.text)
    }

    /// Whether this key names a credential field.
    #[must_use]
    pub fn is_secret_field(&self) -> bool {
        SECRET_FIELD_NAMES.contains(&self.field())
    }
}

impl fmt::Display for SettingKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

fn parse_canonical_id(segment: &str) -> Option<NonZeroU128> {
    if segment.starts_with('0') || !segment.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    segment.parse::<u128>().ok().and_then(NonZeroU128::new)
}

/// Why secret text was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretError {
    Empty,
    TooLong,
}

impl fmt::Display for SecretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "secret must not be empty",
            Self::TooLong => "secret is too long",
        })
    }
}

impl std::error::Error for SecretError {}

/// Credential text (stream key, token, password).
///
/// There is no `Display`; `Debug` prints `Secret(****)`; reading the value
/// requires [`Secret::expose_secret`]. The export encoder never calls it.
#[derive(Clone, Eq, PartialEq)]
pub struct Secret(String);

impl Secret {
    /// Wraps credential text.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError`] when the text is empty or oversized. No
    /// variant carries the text.
    pub fn new(secret: impl Into<String>) -> Result<Self, SecretError> {
        let secret = secret.into();
        if secret.is_empty() {
            return Err(SecretError::Empty);
        }
        if secret.len() > MAX_SECRET_BYTES {
            return Err(SecretError::TooLong);
        }
        Ok(Self(secret))
    }

    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(****)")
    }
}

/// A credential field's content: the secret itself, or the placeholder an
/// export carries in its place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SecretSlot {
    Present(Secret),
    /// The credential was excluded from an export and must be re-entered or
    /// retained from the current settings on import.
    Placeholder,
}

/// The kind of object a [`SettingReference`] names.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReferenceKind {
    Input,
    Scene,
    AudioBus,
    Output,
}

impl ReferenceKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Scene => "scene",
            Self::AudioBus => "audio_bus",
            Self::Output => "output",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        [Self::Input, Self::Scene, Self::AudioBus, Self::Output]
            .into_iter()
            .find(|kind| kind.name() == name)
    }
}

/// A reference from one setting to an input, scene, bus or output id.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SettingReference {
    pub kind: ReferenceKind,
    pub id: NonZeroU128,
}

/// One setting value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Integer(i64),
    Text(String),
    Reference(SettingReference),
    Secret(SecretSlot),
}

/// Why a settings document or entry was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsExportError {
    TooLarge {
        bytes: usize,
        max: usize,
    },
    Syntax {
        offset: usize,
        message: String,
    },
    /// The document's `contract` is not [`SETTINGS_EXPORT_CONTRACT`].
    ContractMismatch,
    TooManyEntries {
        max: usize,
    },
    InvalidKey {
        /// The offending key, truncated to [`MAX_SETTING_KEY_BYTES`].
        key: String,
        error: SettingKeyError,
    },
    InvalidValue {
        key: String,
        reason: &'static str,
    },
}

impl fmt::Display for SettingsExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes, max } => write!(
                formatter,
                "settings export is {bytes} bytes; the limit is {max}"
            ),
            Self::Syntax { offset, message } => {
                write!(
                    formatter,
                    "settings export syntax at byte {offset}: {message}"
                )
            }
            Self::ContractMismatch => write!(
                formatter,
                "settings export contract must be `{SETTINGS_EXPORT_CONTRACT}`"
            ),
            Self::TooManyEntries { max } => {
                write!(formatter, "settings export exceeds {max} entries")
            }
            Self::InvalidKey { key, error } => write!(formatter, "setting `{key}`: {error}"),
            Self::InvalidValue { key, reason } => write!(formatter, "setting `{key}`: {reason}"),
        }
    }
}

impl std::error::Error for SettingsExportError {}

/// A flat, ordered, bounded map of settings.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SettingsSnapshot {
    entries: BTreeMap<SettingKey, SettingValue>,
}

impl SettingsSnapshot {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a snapshot from validated entries.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsExportError`] when an entry is invalid for its key
    /// or the entry bound is exceeded.
    pub fn from_entries(
        entries: impl IntoIterator<Item = (SettingKey, SettingValue)>,
    ) -> Result<Self, SettingsExportError> {
        let mut snapshot = Self::new();
        for (key, value) in entries {
            snapshot.insert(key, value)?;
        }
        Ok(snapshot)
    }

    /// Inserts or replaces one entry.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsExportError::InvalidValue`] when a credential field
    /// does not hold a secret, a secret is stored outside a credential field,
    /// or text is oversized; returns [`SettingsExportError::TooManyEntries`]
    /// when a new key would exceed [`MAX_SETTINGS_ENTRIES`].
    pub fn insert(
        &mut self,
        key: SettingKey,
        value: SettingValue,
    ) -> Result<Option<SettingValue>, SettingsExportError> {
        validate_entry(&key, &value)?;
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_SETTINGS_ENTRIES {
            return Err(SettingsExportError::TooManyEntries {
                max: MAX_SETTINGS_ENTRIES,
            });
        }
        Ok(self.entries.insert(key, value))
    }

    #[must_use]
    pub fn get(&self, key: &SettingKey) -> Option<&SettingValue> {
        self.entries.get(key)
    }

    /// Looks up an entry by key path text.
    #[must_use]
    pub fn get_path(&self, key: &str) -> Option<&SettingValue> {
        SettingKey::parse(key)
            .ok()
            .and_then(|key| self.entries.get(&key))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries in stable key path order.
    pub fn entries(&self) -> impl Iterator<Item = (&SettingKey, &SettingValue)> {
        self.entries.iter()
    }

    /// Every entity with at least one entry.
    #[must_use]
    pub fn entities(&self) -> BTreeSet<EntityKey> {
        self.entries.keys().map(SettingKey::entity).collect()
    }

    /// The entries belonging to one entity.
    pub fn entity_entries(
        &self,
        entity: EntityKey,
    ) -> impl Iterator<Item = (&SettingKey, &SettingValue)> {
        self.entries
            .iter()
            .filter(move |(key, _)| key.entity() == entity)
    }

    /// The `<entity>.name` text, if the entity has one.
    #[must_use]
    pub fn entity_name(&self, entity: EntityKey) -> Option<&str> {
        self.entity_entries(entity)
            .find_map(|(key, value)| match value {
                SettingValue::Text(text) if key.field() == "name" => Some(text.as_str()),
                _ => None,
            })
    }

    /// Extracts the exportable settings of a project.
    ///
    /// Covers the frame rate, video dimensions, audio sample rate and channel
    /// count, restart policy, outputs, audio buses (names), and stream
    /// targets. Stream keys are held as [`SecretSlot::Present`].
    ///
    /// # Errors
    ///
    /// Returns [`SettingsExportError`] when the project exceeds snapshot
    /// bounds.
    pub fn from_project(project: &Project) -> Result<Self, SettingsExportError> {
        let mut snapshot = Self::new();
        let settings = project.settings();
        snapshot.put(
            "format.frame_rate.numerator",
            SettingValue::Integer(settings.frame_rate.numerator().into()),
        )?;
        snapshot.put(
            "format.frame_rate.denominator",
            SettingValue::Integer(settings.frame_rate.denominator().into()),
        )?;
        snapshot.put(
            "format.video.width",
            SettingValue::Integer(settings.video.dimensions.width().into()),
        )?;
        snapshot.put(
            "format.video.height",
            SettingValue::Integer(settings.video.dimensions.height().into()),
        )?;
        snapshot.put(
            "format.audio.sample_rate_hz",
            SettingValue::Integer(settings.audio.sample_rate.hertz().into()),
        )?;
        snapshot.put(
            "format.audio.channel_count",
            SettingValue::Integer(
                i64::try_from(settings.audio.channels.channels().len()).unwrap_or(i64::MAX),
            ),
        )?;
        let (mode, attempts) = match project.restart_policy() {
            RestartPolicy::Never => ("never", None),
            RestartPolicy::OnFailure { max_attempts } => ("on_failure", Some(max_attempts)),
            RestartPolicy::Always => ("always", None),
        };
        snapshot.put("restart_policy.mode", text(mode))?;
        if let Some(attempts) = attempts {
            snapshot.put(
                "restart_policy.max_attempts",
                SettingValue::Integer(attempts.into()),
            )?;
        }
        for output in project.outputs() {
            let prefix = format!("outputs.{}", output.id);
            snapshot.put(&format!("{prefix}.name"), text(&output.name))?;
            snapshot.put(
                &format!("{prefix}.video_source"),
                reference(ReferenceKind::Scene, output.video_source.get()),
            )?;
            snapshot.put(
                &format!("{prefix}.audio_source"),
                reference(ReferenceKind::AudioBus, output.audio_source.get()),
            )?;
            snapshot.put(&format!("{prefix}.startup"), text(startup(output.startup)))?;
        }
        for bus in project.audio_buses() {
            snapshot.put(&format!("audio_buses.{}.name", bus.id), text(&bus.name))?;
        }
        for target in project.stream_targets() {
            let prefix = format!("stream_targets.{}", target.id());
            snapshot.put(&format!("{prefix}.name"), text(target.name()))?;
            snapshot.put(
                &format!("{prefix}.protocol"),
                text(protocol(target.protocol())),
            )?;
            snapshot.put(
                &format!("{prefix}.endpoint"),
                text(target.endpoint().as_str()),
            )?;
            if let Some(backup) = target.backup_endpoint() {
                snapshot.put(&format!("{prefix}.backup_endpoint"), text(backup.as_str()))?;
            }
            if let Some(key) = target.key() {
                let secret = Secret::new(key.expose_secret()).map_err(|_| {
                    SettingsExportError::InvalidValue {
                        key: format!("{prefix}.key"),
                        reason: "stream key exceeds secret bounds",
                    }
                })?;
                snapshot.put(
                    &format!("{prefix}.key"),
                    SettingValue::Secret(SecretSlot::Present(secret)),
                )?;
            }
            snapshot.put(
                &format!("{prefix}.startup"),
                text(startup(target.startup())),
            )?;
            snapshot.put(
                &format!("{prefix}.video_bitrate_kbps"),
                SettingValue::Integer(target.video_bitrate_kbps().into()),
            )?;
            snapshot.put(
                &format!("{prefix}.output"),
                reference(ReferenceKind::Output, target.output().get()),
            )?;
        }
        Ok(snapshot)
    }

    fn put(&mut self, key: &str, value: SettingValue) -> Result<(), SettingsExportError> {
        let key = SettingKey::parse(key).map_err(|error| SettingsExportError::InvalidKey {
            key: truncate(key),
            error,
        })?;
        self.insert(key, value).map(|_| ())
    }

    /// Encodes the snapshot as a secret-free export document.
    ///
    /// Output is deterministic: entries appear in key path order, one per
    /// line. Secret fields are written as `{"secret_ref": "<key path>"}`
    /// regardless of whether the secret is present.
    #[must_use]
    pub fn to_export_json(&self) -> String {
        let mut output = String::new();
        output.push_str("{\n  \"contract\": \"");
        output.push_str(SETTINGS_EXPORT_CONTRACT);
        output.push_str("\",\n  \"entries\": {");
        for (index, (key, value)) in self.entries.iter().enumerate() {
            output.push_str(if index == 0 { "\n    \"" } else { ",\n    \"" });
            output.push_str(key.as_str());
            output.push_str("\": ");
            write_value(&mut output, key, value);
        }
        output.push_str(if self.entries.is_empty() {
            "}\n}\n"
        } else {
            "\n  }\n}\n"
        });
        output
    }

    /// Parses an export document. Secret fields become placeholders.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsExportError`] for oversized documents, JSON syntax
    /// errors, a wrong contract tag, unknown or missing fields, invalid key
    /// paths, malformed values, a `secret_ref` that does not name its own key,
    /// or a credential field carrying a non-secret value.
    pub fn parse_export_json(source: &str) -> Result<Self, SettingsExportError> {
        if source.len() > MAX_SETTINGS_EXPORT_BYTES {
            return Err(SettingsExportError::TooLarge {
                bytes: source.len(),
                max: MAX_SETTINGS_EXPORT_BYTES,
            });
        }
        let document = Reader::new(source)
            .document()
            .map_err(|error| match error {
                DecodeError::Syntax { offset, message } => {
                    SettingsExportError::Syntax { offset, message }
                }
                DecodeError::Validation(error) => SettingsExportError::Syntax {
                    offset: 0,
                    message: error.to_string(),
                },
            })?;
        let Value::Object(mut root) = document else {
            return Err(shape("settings export must be an object"));
        };
        let Some(Value::String(contract)) = root.remove("contract") else {
            return Err(shape("missing string field `contract`"));
        };
        if contract != SETTINGS_EXPORT_CONTRACT {
            return Err(SettingsExportError::ContractMismatch);
        }
        let Some(Value::Object(entries)) = root.remove("entries") else {
            return Err(shape("missing object field `entries`"));
        };
        if let Some(field) = root.keys().next() {
            return Err(shape(format!("unknown field `{field}` in settings export")));
        }
        if entries.len() > MAX_SETTINGS_ENTRIES {
            return Err(SettingsExportError::TooManyEntries {
                max: MAX_SETTINGS_ENTRIES,
            });
        }
        let mut snapshot = Self::new();
        for (text, value) in entries {
            let key =
                SettingKey::parse(&text).map_err(|error| SettingsExportError::InvalidKey {
                    key: truncate(&text),
                    error,
                })?;
            let value = parse_value(&key, value)?;
            snapshot.insert(key, value)?;
        }
        Ok(snapshot)
    }
}

fn validate_entry(key: &SettingKey, value: &SettingValue) -> Result<(), SettingsExportError> {
    let invalid = |reason| SettingsExportError::InvalidValue {
        key: key.as_str().to_owned(),
        reason,
    };
    match value {
        SettingValue::Secret(_) if !key.is_secret_field() => {
            Err(invalid("secrets are only allowed in credential fields"))
        }
        SettingValue::Secret(_) => Ok(()),
        _ if key.is_secret_field() => Err(invalid("credential fields must hold a secret")),
        SettingValue::Text(text) if text.len() > MAX_SETTING_TEXT_BYTES => {
            Err(invalid("text value is too long"))
        }
        _ => Ok(()),
    }
}

fn parse_value(key: &SettingKey, value: Value) -> Result<SettingValue, SettingsExportError> {
    let invalid = |reason| SettingsExportError::InvalidValue {
        key: key.as_str().to_owned(),
        reason,
    };
    let Value::Object(object) = value else {
        return Err(invalid("value must be a single-field object"));
    };
    if object.len() != 1 {
        return Err(invalid("value must be a single-field object"));
    }
    let Some((kind, inner)) = object.into_iter().next() else {
        return Err(invalid("value must be a single-field object"));
    };
    match (kind.as_str(), inner) {
        ("bool", Value::Bool(value)) => Ok(SettingValue::Bool(value)),
        ("integer", Value::Number(value)) => i64::try_from(value)
            .map(SettingValue::Integer)
            .map_err(|_| invalid("integer exceeds i64")),
        ("integer", Value::NegativeNumber(value)) => i128::try_from(value)
            .ok()
            .and_then(|magnitude| i64::try_from(-magnitude).ok())
            .map(SettingValue::Integer)
            .ok_or_else(|| invalid("integer exceeds i64")),
        ("text", Value::String(value)) => Ok(SettingValue::Text(value)),
        ("reference", Value::Object(mut reference)) => {
            let kind = match reference.remove("kind") {
                Some(Value::String(kind)) => ReferenceKind::from_name(&kind)
                    .ok_or_else(|| invalid("unknown reference kind"))?,
                _ => return Err(invalid("reference needs a string `kind`")),
            };
            let id = match reference.remove("id") {
                Some(Value::Number(id)) => {
                    NonZeroU128::new(id).ok_or_else(|| invalid("reference id must be nonzero"))?
                }
                _ => return Err(invalid("reference needs an unsigned `id`")),
            };
            if !reference.is_empty() {
                return Err(invalid("reference has unknown fields"));
            }
            Ok(SettingValue::Reference(SettingReference { kind, id }))
        }
        ("secret_ref", Value::String(reference)) => {
            if reference == key.as_str() {
                Ok(SettingValue::Secret(SecretSlot::Placeholder))
            } else {
                Err(invalid("secret_ref must name its own key path"))
            }
        }
        ("bool" | "integer" | "text" | "reference" | "secret_ref", _) => {
            Err(invalid("value has the wrong JSON type for its kind"))
        }
        _ => Err(invalid("unknown value kind")),
    }
}

fn write_value(output: &mut String, key: &SettingKey, value: &SettingValue) {
    match value {
        SettingValue::Bool(value) => {
            write!(output, "{{\"bool\": {value}}}").expect("writing to a string cannot fail");
        }
        SettingValue::Integer(value) => {
            write!(output, "{{\"integer\": {value}}}").expect("writing to a string cannot fail");
        }
        SettingValue::Text(value) => {
            output.push_str("{\"text\": \"");
            escape_string(output, value);
            output.push_str("\"}");
        }
        SettingValue::Reference(reference) => {
            write!(
                output,
                "{{\"reference\": {{\"kind\": \"{}\", \"id\": {}}}}}",
                reference.kind.name(),
                reference.id
            )
            .expect("writing to a string cannot fail");
        }
        // The slot's content is deliberately not inspected: present and
        // placeholder secrets encode identically.
        SettingValue::Secret(_) => {
            output.push_str("{\"secret_ref\": \"");
            output.push_str(key.as_str());
            output.push_str("\"}");
        }
    }
}

fn shape(message: impl Into<String>) -> SettingsExportError {
    SettingsExportError::Syntax {
        offset: 0,
        message: message.into(),
    }
}

fn truncate(text: &str) -> String {
    let mut end = text.len().min(MAX_SETTING_KEY_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn text(value: &str) -> SettingValue {
    SettingValue::Text(value.to_owned())
}

const fn reference(kind: ReferenceKind, id: NonZeroU128) -> SettingValue {
    SettingValue::Reference(SettingReference { kind, id })
}

const fn startup(policy: StartupPolicy) -> &'static str {
    match policy {
        StartupPolicy::Stopped => "stopped",
        StartupPolicy::ReconcileDesiredState => "reconcile_desired_state",
    }
}

const fn protocol(protocol: StreamProtocol) -> &'static str {
    protocol.scheme()
}
