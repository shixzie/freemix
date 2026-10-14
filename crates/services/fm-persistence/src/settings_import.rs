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

use fm_model::Project;

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
