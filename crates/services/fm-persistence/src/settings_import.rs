//! Settings import planning: change preview, merge strategy, conflict report,
//! and atomic apply.
//!
//! An [`ImportPlan`] is computed against one base [`SettingsSnapshot`]. It
//! never mutates anything until [`ImportPlan::apply`], which refuses to run
//! against a different base, refuses while any conflict is unresolved, and
//! otherwise replaces the caller's snapshot in one assignment.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    num::NonZeroU128,
};

use fm_model::{
    AudioBus, Output, Project, ProjectSettings, RestartPolicy, StartupPolicy, StreamEndpoint,
};
use fm_types::{
    AudioFormat, BusId, Channel, ChannelLayout, DiscreteChannel, FrameRate, OutputId, SampleRate,
    SceneId, VideoDimensions, VideoFormat,
};

use crate::settings_export::{
    EntityKey, ReferenceKind, Secret, SecretSlot, SettingKey, SettingReference, SettingValue,
    SettingsExportError, SettingsSection, SettingsSnapshot,
};

/// How one key path differs between two snapshots.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ChangeKind {
    Added,
    Changed,
    Removed,
    Unchanged,
}

/// One key path's change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyChange {
    pub key: SettingKey,
    pub kind: ChangeKind,
}

/// A per-key-path comparison of two snapshots, in key path order.
///
/// Secrets are compared by value only when both sides hold the secret; a
/// placeholder on either side compares as unchanged, because an export never
/// reveals what the excluded secret was.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChangePreview {
    changes: Vec<KeyChange>,
}

impl ChangePreview {
    #[must_use]
    pub fn between(before: &SettingsSnapshot, after: &SettingsSnapshot) -> Self {
        let keys: BTreeSet<&SettingKey> = before
            .entries()
            .map(|(key, _)| key)
            .chain(after.entries().map(|(key, _)| key))
            .collect();
        let changes = keys
            .into_iter()
            .map(|key| {
                let kind = match (before.get(key), after.get(key)) {
                    (None, Some(_)) => ChangeKind::Added,
                    (Some(_), None) => ChangeKind::Removed,
                    (Some(left), Some(right)) if equivalent(left, right) => ChangeKind::Unchanged,
                    _ => ChangeKind::Changed,
                };
                KeyChange {
                    key: key.clone(),
                    kind,
                }
            })
            .collect();
        Self { changes }
    }

    #[must_use]
    pub fn changes(&self) -> &[KeyChange] {
        &self.changes
    }

    #[must_use]
    pub fn count(&self, kind: ChangeKind) -> usize {
        self.changes
            .iter()
            .filter(|change| change.kind == kind)
            .count()
    }

    /// The change for one key path, if the path appears on either side.
    #[must_use]
    pub fn kind_of(&self, key: &str) -> Option<ChangeKind> {
        self.changes
            .iter()
            .find(|change| change.key.as_str() == key)
            .map(|change| change.kind)
    }

    #[must_use]
    pub fn has_changes(&self) -> bool {
        self.changes
            .iter()
            .any(|change| change.kind != ChangeKind::Unchanged)
    }
}

fn equivalent(left: &SettingValue, right: &SettingValue) -> bool {
    match (left, right) {
        (
            SettingValue::Secret(SecretSlot::Present(left)),
            SettingValue::Secret(SecretSlot::Present(right)),
        ) => left == right,
        (SettingValue::Secret(_), SettingValue::Secret(_)) => true,
        _ => left == right,
    }
}

/// How one section combines current and incoming entities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SectionChoice {
    /// Ignore the incoming section entirely.
    KeepCurrent,
    /// The section becomes exactly the incoming entities.
    TakeIncoming,
    /// Union of entities; incoming wins where both define one.
    PreferIncoming,
    /// Union of entities; current wins where both define one.
    PreferCurrent,
}

/// The import merge strategy. Entities are merged whole: a merged entity
/// never mixes fields from both sides.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MergeStrategy {
    /// Every section becomes exactly the incoming settings.
    ReplaceAll,
    MergePreferIncoming,
    MergePreferCurrent,
    /// Explicit per-section choices; unlisted sections keep current.
    PerSection(BTreeMap<SettingsSection, SectionChoice>),
}

impl MergeStrategy {
    #[must_use]
    pub fn choice(&self, section: SettingsSection) -> SectionChoice {
        match self {
            Self::ReplaceAll => SectionChoice::TakeIncoming,
            Self::MergePreferIncoming => SectionChoice::PreferIncoming,
            Self::MergePreferCurrent => SectionChoice::PreferCurrent,
            Self::PerSection(choices) => choices
                .get(&section)
                .copied()
                .unwrap_or(SectionChoice::KeepCurrent),
        }
    }
}

/// Ids that settings may reference but do not themselves define: project
/// inputs and scenes. Outputs and audio buses are checked against the merged
/// settings instead.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContentInventory {
    inputs: BTreeSet<NonZeroU128>,
    scenes: BTreeSet<NonZeroU128>,
}

impl ContentInventory {
    #[must_use]
    pub fn new(
        inputs: impl IntoIterator<Item = NonZeroU128>,
        scenes: impl IntoIterator<Item = NonZeroU128>,
    ) -> Self {
        Self {
            inputs: inputs.into_iter().collect(),
            scenes: scenes.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn from_project(project: &Project) -> Self {
        Self::new(
            project.inputs().iter().map(|input| input.id.get()),
            project.scenes().iter().map(|scene| scene.id.get()),
        )
    }
}

/// One reason an import cannot be applied as planned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportConflict {
    /// An imported credential was excluded from the export and the current
    /// settings hold none to retain. Resolve with
    /// [`ImportResolutions::with_secret`] or by skipping the entity.
    SecretReentryRequired { key: SettingKey },
    /// The merged settings reference an input, scene, bus, or output that
    /// would not exist. Resolve by skipping the imported entity or choosing a
    /// strategy that keeps the referenced object.
    UnknownReference {
        key: SettingKey,
        reference: SettingReference,
    },
    /// An imported entity would overwrite a current entity with the same id
    /// but a different name. Resolve with
    /// [`ImportResolutions::accept_collision`] or by skipping the entity.
    IdCollision {
        entity: EntityKey,
        current_name: String,
        incoming_name: String,
    },
}

impl ImportConflict {
    /// The entity the conflict is attached to.
    #[must_use]
    pub fn entity(&self) -> EntityKey {
        match self {
            Self::SecretReentryRequired { key } | Self::UnknownReference { key, .. } => {
                key.entity()
            }
            Self::IdCollision { entity, .. } => *entity,
        }
    }
}

impl fmt::Display for ImportConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SecretReentryRequired { key } => {
                write!(formatter, "`{key}` needs its secret re-entered")
            }
            Self::UnknownReference { key, reference } => write!(
                formatter,
                "`{key}` references unknown {} {}",
                reference.kind.name(),
                reference.id
            ),
            Self::IdCollision {
                entity,
                current_name,
                incoming_name,
            } => write!(
                formatter,
                "`{entity}` is `{current_name}` here but `{incoming_name}` in the import"
            ),
        }
    }
}

/// Operator decisions that resolve conflicts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportResolutions {
    secrets: BTreeMap<SettingKey, Secret>,
    skipped: BTreeSet<EntityKey>,
    accepted_collisions: BTreeSet<EntityKey>,
}

impl ImportResolutions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Supplies a re-entered (or rotated) secret for an imported credential.
    #[must_use]
    pub fn with_secret(mut self, key: SettingKey, secret: Secret) -> Self {
        self.secrets.insert(key, secret);
        self
    }

    /// Does not import this entity; the current entity, if any, is kept.
    #[must_use]
    pub fn skip_entity(mut self, entity: EntityKey) -> Self {
        self.skipped.insert(entity);
        self
    }

    /// Accepts that an imported entity overwrites a differently named one.
    #[must_use]
    pub fn accept_collision(mut self, entity: EntityKey) -> Self {
        self.accepted_collisions.insert(entity);
        self
    }
}

/// Why an import could not be planned or applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportError {
    Export(SettingsExportError),
    /// The snapshot passed to [`ImportPlan::apply`] is not the plan's base.
    StaleBase,
    /// Conflicts remain; nothing was changed.
    UnresolvedConflicts(Vec<ImportConflict>),
    /// A secret was supplied for a key that is not an imported credential.
    UnexpectedSecret {
        key: SettingKey,
    },
    /// A skipped entity does not appear in the import.
    UnknownSkip {
        entity: EntityKey,
    },
    /// An accepted collision is not an id collision in this plan.
    UnknownCollision {
        entity: EntityKey,
    },
}

impl fmt::Display for ImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Export(error) => error.fmt(formatter),
            Self::StaleBase => formatter.write_str("settings changed since the import was planned"),
            Self::UnresolvedConflicts(conflicts) => write!(
                formatter,
                "{} import conflict(s) remain unresolved",
                conflicts.len()
            ),
            Self::UnexpectedSecret { key } => {
                write!(formatter, "`{key}` is not an imported credential")
            }
            Self::UnknownSkip { entity } => {
                write!(formatter, "`{entity}` is not part of the import")
            }
            Self::UnknownCollision { entity } => {
                write!(formatter, "`{entity}` is not an id collision")
            }
        }
    }
}

impl std::error::Error for ImportError {}

impl From<SettingsExportError> for ImportError {
    fn from(error: SettingsExportError) -> Self {
        Self::Export(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Origin {
    Current,
    Incoming,
}

/// The outcome of resolving a plan: the would-be settings, the conflicts that
/// remain, and the credentials carried over from the current settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportOutcome {
    pub result: SettingsSnapshot,
    pub preview: ChangePreview,
    pub conflicts: Vec<ImportConflict>,
    /// Imported credential placeholders filled from the current settings.
    pub retained_secrets: Vec<SettingKey>,
}

/// A deterministic import plan against one base snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportPlan {
    base: SettingsSnapshot,
    incoming: SettingsSnapshot,
    inventory: ContentInventory,
    strategy: MergeStrategy,
}

impl ImportPlan {
    #[must_use]
    pub fn new(
        current: &SettingsSnapshot,
        incoming: SettingsSnapshot,
        inventory: ContentInventory,
        strategy: MergeStrategy,
    ) -> Self {
        Self {
            base: current.clone(),
            incoming,
            inventory,
            strategy,
        }
    }

    /// Parses an export document and plans its import.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::Export`] when the document is refused.
    pub fn from_export_json(
        current: &SettingsSnapshot,
        source: &str,
        inventory: ContentInventory,
        strategy: MergeStrategy,
    ) -> Result<Self, ImportError> {
        let incoming = SettingsSnapshot::parse_export_json(source)?;
        Ok(Self::new(current, incoming, inventory, strategy))
    }

    #[must_use]
    pub const fn incoming(&self) -> &SettingsSnapshot {
        &self.incoming
    }

    /// Raw comparison of the current settings with the import, independent
    /// of the merge strategy.
    #[must_use]
    pub fn incoming_preview(&self) -> ChangePreview {
        ChangePreview::between(&self.base, &self.incoming)
    }

    /// Resolves the plan with no operator decisions.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::Export`] if the merged settings exceed bounds.
    pub fn outcome(&self) -> Result<ImportOutcome, ImportError> {
        self.resolve(&ImportResolutions::default())
    }

    /// Resolves the plan with operator decisions, without changing anything.
    ///
    /// # Errors
    ///
    /// Returns an [`ImportError`] when a resolution does not match the plan
    /// (unexpected secret, unknown skip, unknown collision) or the merged
    /// settings exceed bounds.
    pub fn resolve(&self, resolutions: &ImportResolutions) -> Result<ImportOutcome, ImportError> {
        let incoming_entities = self.incoming.entities();
        if let Some(entity) = resolutions
            .skipped
            .iter()
            .find(|entity| !incoming_entities.contains(entity))
        {
            return Err(ImportError::UnknownSkip { entity: *entity });
        }
        let origins = self.origins(&resolutions.skipped);

        let mut result = SettingsSnapshot::new();
        let mut conflicts = Vec::new();
        let mut retained_secrets = Vec::new();
        let mut used_secrets = BTreeSet::new();
        for (entity, origin) in &origins {
            let source = match origin {
                Origin::Current => &self.base,
                Origin::Incoming => &self.incoming,
            };
            for (key, value) in source.entity_entries(*entity) {
                let value = match (origin, value) {
                    (Origin::Incoming, SettingValue::Secret(_)) => {
                        if let Some(secret) = resolutions.secrets.get(key) {
                            used_secrets.insert(key.clone());
                            SettingValue::Secret(SecretSlot::Present(secret.clone()))
                        } else if let SettingValue::Secret(SecretSlot::Present(_)) = value {
                            value.clone()
                        } else if let Some(SettingValue::Secret(SecretSlot::Present(secret))) =
                            self.base.get(key)
                        {
                            retained_secrets.push(key.clone());
                            SettingValue::Secret(SecretSlot::Present(secret.clone()))
                        } else {
                            conflicts
                                .push(ImportConflict::SecretReentryRequired { key: key.clone() });
                            value.clone()
                        }
                    }
                    _ => value.clone(),
                };
                result.insert(key.clone(), value)?;
            }
        }
        if let Some(key) = resolutions
            .secrets
            .keys()
            .find(|key| !used_secrets.contains(*key))
        {
            return Err(ImportError::UnexpectedSecret { key: key.clone() });
        }

        self.collisions(&origins, resolutions, &mut conflicts)?;
        self.references(&result, &mut conflicts);

        Ok(ImportOutcome {
            preview: ChangePreview::between(&self.base, &result),
            result,
            conflicts,
            retained_secrets,
        })
    }

    /// Applies the plan to `current` only if it is still the plan's base and
    /// no conflict remains; otherwise `current` is left untouched.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::StaleBase`], any resolution error from
    /// [`ImportPlan::resolve`], or [`ImportError::UnresolvedConflicts`].
    pub fn apply(
        &self,
        current: &mut SettingsSnapshot,
        resolutions: &ImportResolutions,
    ) -> Result<ChangePreview, ImportError> {
        if *current != self.base {
            return Err(ImportError::StaleBase);
        }
        let outcome = self.resolve(resolutions)?;
        if !outcome.conflicts.is_empty() {
            return Err(ImportError::UnresolvedConflicts(outcome.conflicts));
        }
        *current = outcome.result;
        Ok(outcome.preview)
    }

    fn collisions(
        &self,
        origins: &BTreeMap<EntityKey, Origin>,
        resolutions: &ImportResolutions,
        conflicts: &mut Vec<ImportConflict>,
    ) -> Result<(), ImportError> {
        let mut collisions = BTreeSet::new();
        for (entity, origin) in origins {
            if *origin != Origin::Incoming || entity.id().is_none() {
                continue;
            }
            let (Some(current_name), Some(incoming_name)) = (
                self.base.entity_name(*entity),
                self.incoming.entity_name(*entity),
            ) else {
                continue;
            };
            if current_name == incoming_name {
                continue;
            }
            collisions.insert(*entity);
            if !resolutions.accepted_collisions.contains(entity) {
                conflicts.push(ImportConflict::IdCollision {
                    entity: *entity,
                    current_name: current_name.to_owned(),
                    incoming_name: incoming_name.to_owned(),
                });
            }
        }
        if let Some(entity) = resolutions
            .accepted_collisions
            .iter()
            .find(|entity| !collisions.contains(entity))
        {
            return Err(ImportError::UnknownCollision { entity: *entity });
        }
        Ok(())
    }

    fn references(&self, result: &SettingsSnapshot, conflicts: &mut Vec<ImportConflict>) {
        let defined = result.entities();
        let is_defined = |section, id| {
            defined.contains(&EntityKey::new(section, Some(id)).expect("collection section"))
        };
        for (key, value) in result.entries() {
            let SettingValue::Reference(reference) = value else {
                continue;
            };
            let known = match reference.kind {
                ReferenceKind::Input => self.inventory.inputs.contains(&reference.id),
                ReferenceKind::Scene => self.inventory.scenes.contains(&reference.id),
                ReferenceKind::AudioBus => is_defined(SettingsSection::AudioBuses, reference.id),
                ReferenceKind::Output => is_defined(SettingsSection::Outputs, reference.id),
            };
            if !known {
                conflicts.push(ImportConflict::UnknownReference {
                    key: key.clone(),
                    reference: *reference,
                });
            }
        }
    }

    fn origins(&self, skipped: &BTreeSet<EntityKey>) -> BTreeMap<EntityKey, Origin> {
        let current = self.base.entities();
        let incoming = self.incoming.entities();
        let mut origins = BTreeMap::new();
        for section in SettingsSection::ALL {
            let in_section = |entity: &&EntityKey| entity.section() == section;
            let current_entities = current.iter().filter(in_section);
            let incoming_entities = incoming.iter().filter(in_section);
            match self.strategy.choice(section) {
                SectionChoice::KeepCurrent => {
                    origins.extend(current_entities.map(|entity| (*entity, Origin::Current)));
                }
                SectionChoice::TakeIncoming => {
                    origins.extend(incoming_entities.map(|entity| (*entity, Origin::Incoming)));
                }
                SectionChoice::PreferIncoming => {
                    origins.extend(current_entities.map(|entity| (*entity, Origin::Current)));
                    origins.extend(incoming_entities.map(|entity| (*entity, Origin::Incoming)));
                }
                SectionChoice::PreferCurrent => {
                    origins.extend(incoming_entities.map(|entity| (*entity, Origin::Incoming)));
                    origins.extend(current_entities.map(|entity| (*entity, Origin::Current)));
                }
            }
        }
        for entity in skipped {
            if origins.get(entity) == Some(&Origin::Incoming) {
                if current.contains(entity) {
                    origins.insert(*entity, Origin::Current);
                } else {
                    origins.remove(entity);
                }
            }
        }
        origins
    }
}

/// Why a merged settings snapshot could not be applied to a project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsApplyError {
    /// A required key is missing from the snapshot.
    Missing { key: String },
    /// A key holds the wrong value type or an out-of-range value.
    Invalid { key: String },
    /// The project model refused an entity edit.
    Model { key: String, reason: String },
    /// The project rejected the new format.
    Project(Vec<fm_model::ValidationError>),
}

impl fmt::Display for SettingsApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { key } => write!(formatter, "settings snapshot has no `{key}`"),
            Self::Invalid { key } => write!(formatter, "setting `{key}` is not applicable"),
            Self::Model { key, reason } => {
                write!(formatter, "setting `{key}` was refused: {reason}")
            }
            Self::Project(errors) => {
                write!(formatter, "project rejected the imported settings: {errors:?}")
            }
        }
    }
}

impl std::error::Error for SettingsApplyError {}

impl SettingsSnapshot {
    /// Applies the format, restart policy, audio bus, output, and existing
    /// stream-target sections to `project`.
    ///
    /// A stream-target addition or removal is refused; a change to an
    /// existing target must keep its protocol so unexported options keep
    /// their meaning. The whole import is applied to a clone and committed
    /// only when every section succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsApplyError`] for a missing or malformed entry, a
    /// model refusal, or an invalid resulting format; nothing changes on
    /// error.
    pub fn apply_exported(&self, project: &mut Project) -> Result<(), SettingsApplyError> {
        let mut candidate = project.clone();
        apply_format(self, &mut candidate)?;
        apply_restart(self, &mut candidate)?;
        apply_audio_buses(self, &mut candidate)?;
        apply_outputs(self, &mut candidate)?;
        apply_stream_targets(self, &mut candidate)?;
        *project = candidate;
        Ok(())
    }
}

fn setting_integer(
    snapshot: &SettingsSnapshot,
    key: &str,
) -> Result<i64, SettingsApplyError> {
    match snapshot.get_path(key) {
        Some(SettingValue::Integer(value)) => Ok(*value),
        Some(_) => Err(SettingsApplyError::Invalid { key: key.to_owned() }),
        None => Err(SettingsApplyError::Missing { key: key.to_owned() }),
    }
}

fn setting_text<'a>(
    snapshot: &'a SettingsSnapshot,
    key: &str,
) -> Result<&'a str, SettingsApplyError> {
    match snapshot.get_path(key) {
        Some(SettingValue::Text(value)) => Ok(value.as_str()),
        Some(_) => Err(SettingsApplyError::Invalid { key: key.to_owned() }),
        None => Err(SettingsApplyError::Missing { key: key.to_owned() }),
    }
}

fn setting_reference(
    snapshot: &SettingsSnapshot,
    key: &str,
    kind: ReferenceKind,
) -> Result<SettingReference, SettingsApplyError> {
    match snapshot.get_path(key) {
        Some(SettingValue::Reference(reference)) if reference.kind == kind => Ok(*reference),
        Some(_) => Err(SettingsApplyError::Invalid { key: key.to_owned() }),
        None => Err(SettingsApplyError::Missing { key: key.to_owned() }),
    }
}

fn bounded_u32(value: i64, key: &str) -> Result<u32, SettingsApplyError> {
    u32::try_from(value).map_err(|_| SettingsApplyError::Invalid { key: key.to_owned() })
}

fn apply_format(
    snapshot: &SettingsSnapshot,
    project: &mut Project,
) -> Result<(), SettingsApplyError> {
    let numerator = bounded_u32(
        setting_integer(snapshot, "format.frame_rate.numerator")?,
        "format.frame_rate.numerator",
    )?;
    let denominator = bounded_u32(
        setting_integer(snapshot, "format.frame_rate.denominator")?,
        "format.frame_rate.denominator",
    )?;
    let frame_rate =
        FrameRate::new(numerator, denominator).map_err(|_| SettingsApplyError::Invalid {
            key: "format.frame_rate".to_owned(),
        })?;
    let width = bounded_u32(
        setting_integer(snapshot, "format.video.width")?,
        "format.video.width",
    )?;
    let height = bounded_u32(
        setting_integer(snapshot, "format.video.height")?,
        "format.video.height",
    )?;
    let dimensions =
        VideoDimensions::new(width, height).ok_or(SettingsApplyError::Invalid {
            key: "format.video.width".to_owned(),
        })?;
    let sample_rate = SampleRate::new(bounded_u32(
        setting_integer(snapshot, "format.audio.sample_rate_hz")?,
        "format.audio.sample_rate_hz",
    )?)
    .ok_or(SettingsApplyError::Invalid {
        key: "format.audio.sample_rate_hz".to_owned(),
    })?;
    let channel_count =
        usize::try_from(setting_integer(snapshot, "format.audio.channel_count")?)
            .ok()
            .filter(|count| (1..=ChannelLayout::MAX_CHANNELS).contains(count))
            .ok_or(SettingsApplyError::Invalid {
                key: "format.audio.channel_count".to_owned(),
            })?;

    let current = project.settings().clone();
    let channels = if channel_count == current.audio.channels.channels().len() {
        current.audio.channels.clone()
    } else {
        let labels = (1..=channel_count)
            .map(|number| {
                u8::try_from(number)
                    .ok()
                    .and_then(DiscreteChannel::new)
                    .map(Channel::Discrete)
                    .ok_or(SettingsApplyError::Invalid {
                        key: "format.audio.channel_count".to_owned(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        ChannelLayout::new(labels).ok_or(SettingsApplyError::Invalid {
            key: "format.audio.channel_count".to_owned(),
        })?
    };
    let settings = ProjectSettings {
        frame_rate,
        video: VideoFormat {
            dimensions,
            frame_rate,
            ..current.video
        },
        audio: AudioFormat {
            sample_rate,
            channels,
            ..current.audio
        },
    };
    project
        .set_settings(settings)
        .map_err(SettingsApplyError::Project)
}

fn apply_restart(
    snapshot: &SettingsSnapshot,
    project: &mut Project,
) -> Result<(), SettingsApplyError> {
    let restart = match setting_text(snapshot, "restart_policy.mode")? {
        "never" => RestartPolicy::Never,
        "always" => RestartPolicy::Always,
        "on_failure" => {
            let attempts = u8::try_from(setting_integer(
                snapshot,
                "restart_policy.max_attempts",
            )?)
            .ok()
            .filter(|attempts| {
                *attempts > 0 && *attempts <= RestartPolicy::MAX_RESTART_ATTEMPTS
            })
            .ok_or(SettingsApplyError::Invalid {
                key: "restart_policy.max_attempts".to_owned(),
            })?;
            RestartPolicy::OnFailure {
                max_attempts: attempts,
            }
        }
        _ => {
            return Err(SettingsApplyError::Invalid {
                key: "restart_policy.mode".to_owned(),
            });
        }
    };
    project.set_restart_policy(restart);
    Ok(())
}

fn apply_audio_buses(
    snapshot: &SettingsSnapshot,
    project: &mut Project,
) -> Result<(), SettingsApplyError> {
    for entity in snapshot
        .entities()
        .into_iter()
        .filter(|entity| entity.section() == SettingsSection::AudioBuses)
    {
        let Some(id) = entity.id() else {
            return Err(SettingsApplyError::Invalid {
                key: "audio_buses".to_owned(),
            });
        };
        let bus = BusId::new(id);
        let key = format!("audio_buses.{id}");
        let name = snapshot
            .entity_name(entity)
            .ok_or_else(|| SettingsApplyError::Missing {
                key: format!("{key}.name"),
            })?
            .to_owned();
        if project.audio_buses().iter().any(|candidate| candidate.id == bus) {
            project
                .rename_audio_bus(bus, name)
                .map_err(|error| SettingsApplyError::Model {
                    key: key.clone(),
                    reason: error.to_string(),
                })?;
        } else {
            project
                .add_audio_bus_checked(AudioBus {
                    id: bus,
                    name,
                    sends: Vec::new(),
                })
                .map_err(|error| SettingsApplyError::Model {
                    key: key.clone(),
                    reason: error.to_string(),
                })?;
        }
    }
    Ok(())
}

fn apply_outputs(
    snapshot: &SettingsSnapshot,
    project: &mut Project,
) -> Result<(), SettingsApplyError> {
    for entity in snapshot
        .entities()
        .into_iter()
        .filter(|entity| entity.section() == SettingsSection::Outputs)
    {
        let Some(id) = entity.id() else {
            return Err(SettingsApplyError::Invalid {
                key: "outputs".to_owned(),
            });
        };
        let output = OutputId::new(id);
        let key = format!("outputs.{id}");
        let name = snapshot
            .entity_name(entity)
            .ok_or_else(|| SettingsApplyError::Missing {
                key: format!("{key}.name"),
            })?
            .to_owned();
        let scene = SceneId::new(
            setting_reference(
                snapshot,
                &format!("{key}.video_source"),
                ReferenceKind::Scene,
            )?
            .id,
        );
        let bus = BusId::new(
            setting_reference(
                snapshot,
                &format!("{key}.audio_source"),
                ReferenceKind::AudioBus,
            )?
            .id,
        );
        let startup = startup_policy(snapshot, &format!("{key}.startup"))?;
        if project.outputs().iter().any(|candidate| candidate.id == output) {
            project
                .rename_output(output, name)
                .map_err(|error| SettingsApplyError::Model {
                    key: key.clone(),
                    reason: error.to_string(),
                })?;
            project
                .set_output_route(output, scene, bus)
                .map_err(|error| SettingsApplyError::Model {
                    key: key.clone(),
                    reason: error.to_string(),
                })?;
            project
                .set_output_startup(output, startup)
                .map_err(|error| SettingsApplyError::Model {
                    key: key.clone(),
                    reason: error.to_string(),
                })?;
        } else {
            project
                .add_output_checked(Output {
                    id: output,
                    name,
                    video_source: scene,
                    audio_source: bus,
                    startup,
                    required_capabilities: Vec::new(),
                })
                .map_err(|error| SettingsApplyError::Model {
                    key: key.clone(),
                    reason: error.to_string(),
                })?;
        }
    }
    Ok(())
}

fn apply_stream_targets(
    snapshot: &SettingsSnapshot,
    project: &mut Project,
) -> Result<(), SettingsApplyError> {
    let mut present = BTreeSet::new();
    for entity in snapshot
        .entities()
        .into_iter()
        .filter(|entity| entity.section() == SettingsSection::StreamTargets)
    {
        let Some(id) = entity.id() else {
            return Err(SettingsApplyError::Invalid {
                key: "stream_targets".to_owned(),
            });
        };
        present.insert(id);
        let key = format!("stream_targets.{id}");
        let Some(target) = project
            .stream_targets()
            .iter()
            .find(|target| target.id().get() == id)
            .cloned()
        else {
            return Err(SettingsApplyError::Model {
                key,
                reason: "adding stream targets is not applied yet".to_owned(),
            });
        };
        let name = snapshot
            .entity_name(entity)
            .ok_or_else(|| SettingsApplyError::Missing {
                key: format!("{key}.name"),
            })?
            .to_owned();
        let protocol = setting_text(snapshot, &format!("{key}.protocol"))?;
        if protocol != target.protocol().scheme() {
            return Err(SettingsApplyError::Model {
                key,
                reason: "changing a stream target's protocol is not applied yet".to_owned(),
            });
        }
        let endpoint = StreamEndpoint::parse_for(
            target.protocol(),
            setting_text(snapshot, &format!("{key}.endpoint"))?,
        )
        .map_err(|_| SettingsApplyError::Invalid {
            key: format!("{key}.endpoint"),
        })?;
        let backup = match snapshot.get_path(&format!("{key}.backup_endpoint")) {
            Some(SettingValue::Text(text)) => {
                Some(StreamEndpoint::parse_for(target.protocol(), text).map_err(|_| {
                    SettingsApplyError::Invalid {
                        key: format!("{key}.backup_endpoint"),
                    }
                })?)
            }
            Some(_) => {
                return Err(SettingsApplyError::Invalid {
                    key: format!("{key}.backup_endpoint"),
                });
            }
            None => None,
        };
        let startup = startup_policy(snapshot, &format!("{key}.startup"))?;
        let bitrate = bounded_u32(
            setting_integer(snapshot, &format!("{key}.video_bitrate_kbps"))?,
            &format!("{key}.video_bitrate_kbps"),
        )?;
        let output = OutputId::new(
            setting_reference(snapshot, &format!("{key}.output"), ReferenceKind::Output)?.id,
        );
        let mut updated = target;
        updated
            .apply_exported(name, endpoint, backup, startup, bitrate, output)
            .map_err(|error| SettingsApplyError::Model {
                key: key.clone(),
                reason: error.to_string(),
            })?;
        project
            .replace_stream_target(updated)
            .map_err(|error| SettingsApplyError::Model {
                key: key.clone(),
                reason: error.to_string(),
            })?;
    }
    for target in project.stream_targets() {
        if !present.contains(&target.id().get()) {
            return Err(SettingsApplyError::Model {
                key: format!("stream_targets.{}", target.id().get()),
                reason: "removing stream targets is not applied yet".to_owned(),
            });
        }
    }
    Ok(())
}

fn startup_policy(
    snapshot: &SettingsSnapshot,
    key: &str,
) -> Result<StartupPolicy, SettingsApplyError> {
    match setting_text(snapshot, key)? {
        "stopped" => Ok(StartupPolicy::Stopped),
        "reconcile_desired_state" => Ok(StartupPolicy::ReconcileDesiredState),
        _ => Err(SettingsApplyError::Invalid { key: key.to_owned() }),
    }
}
