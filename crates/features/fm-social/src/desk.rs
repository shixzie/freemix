//! Role-gated moderation desk: audited commands, hide/feature/rotation, and
//! typed title-field output for the on-air message.
//!
//! Everything here is deterministic. Time is supplied by the caller and must be
//! non-decreasing; every container is bounded by [`DeskConfig`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;

use fm_titles::{FieldId, FieldValue, TitleScene, UpdateError};

use crate::{
    AccountId, AuthorId, IncomingMessage, IngestOutcome, MessageKey, ModerationState, OperatorId,
    PollReport, ProviderError, ProviderId, SocialAggregator, SocialError, SocialMessage,
    SocialProvider, Timestamp, TitleSourceField,
};

/// Operator privilege, ordered from least to most privileged.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Role {
    /// Read-only access to the queue, on-air state, and audit log.
    Viewer,
    /// Approves, rejects, hides, unhides, and blocks authors.
    Moderator,
    /// Moderator rights plus featuring and rotation control.
    Producer,
    /// Producer rights plus unblocking authors and managing operators.
    Admin,
}

/// One operator request against the desk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeskCommand {
    Approve(MessageKey),
    Reject {
        key: MessageKey,
        reason: String,
    },
    /// Blocks the author of a pending message on its provider account.
    Block(MessageKey),
    Unblock {
        provider_id: ProviderId,
        account_id: AccountId,
        author_id: AuthorId,
    },
    /// Withdraws an approved message from air and rotation without deleting it.
    Hide(MessageKey),
    Unhide(MessageKey),
    /// Pins an approved, visible message on air until cleared.
    Feature(MessageKey),
    ClearOnAir,
    AddToRotation(MessageKey),
    RemoveFromRotation(MessageKey),
    SetRole {
        operator: OperatorId,
        role: Role,
    },
    RemoveOperator(OperatorId),
}

impl DeskCommand {
    /// The least privileged role permitted to run this command.
    #[must_use]
    pub const fn required_role(&self) -> Role {
        match self {
            Self::Approve(_)
            | Self::Reject { .. }
            | Self::Block(_)
            | Self::Hide(_)
            | Self::Unhide(_) => Role::Moderator,
            Self::Feature(_)
            | Self::ClearOnAir
            | Self::AddToRotation(_)
            | Self::RemoveFromRotation(_) => Role::Producer,
            Self::Unblock { .. } | Self::SetRole { .. } | Self::RemoveOperator(_) => Role::Admin,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OnAirSource {
    /// Pinned by [`DeskCommand::Feature`]; rotation does not replace it.
    Manual,
    /// Selected by [`ModerationDesk::tick`]; replaced after the dwell elapses.
    Rotation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OnAir {
    pub key: MessageKey,
    pub since: Timestamp,
    pub source: OnAirSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeskError {
    UnknownOperator(OperatorId),
    PermissionDenied {
        operator: OperatorId,
        role: Role,
        required: Role,
    },
    Social(SocialError),
    NotApproved(MessageKey),
    Hidden(MessageKey),
    NotHidden(MessageKey),
    AlreadyInRotation(MessageKey),
    NotInRotation(MessageKey),
    RotationFull {
        capacity: usize,
    },
    AuthorNotBlocked,
    OperatorLimit {
        capacity: usize,
    },
    /// Removing or demoting this operator would leave the desk without an admin.
    LastAdmin(OperatorId),
    /// The supplied time precedes the last time the desk observed.
    ClockRegression {
        last: Timestamp,
        now: Timestamp,
    },
}

impl fmt::Display for DeskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOperator(operator) => write!(formatter, "unknown operator {operator}"),
            Self::PermissionDenied {
                operator,
                role,
                required,
            } => write!(
                formatter,
                "operator {operator} has role {role:?}; {required:?} is required"
            ),
            Self::Social(error) => error.fmt(formatter),
            Self::NotApproved(key) => write!(formatter, "message is not approved: {key:?}"),
            Self::Hidden(key) => write!(formatter, "message is hidden: {key:?}"),
            Self::NotHidden(key) => write!(formatter, "message is not hidden: {key:?}"),
            Self::AlreadyInRotation(key) => {
                write!(formatter, "message is already in rotation: {key:?}")
            }
            Self::NotInRotation(key) => write!(formatter, "message is not in rotation: {key:?}"),
            Self::RotationFull { capacity } => {
                write!(formatter, "rotation is full (capacity {capacity})")
            }
            Self::AuthorNotBlocked => formatter.write_str("author is not blocked"),
            Self::OperatorLimit { capacity } => {
                write!(formatter, "operator limit reached (capacity {capacity})")
            }
            Self::LastAdmin(operator) => {
                write!(formatter, "operator {operator} is the last admin")
            }
            Self::ClockRegression { last, now } => write!(
                formatter,
                "time {} precedes last observed time {}",
                now.0, last.0
            ),
        }
    }
}

impl Error for DeskError {}

impl From<SocialError> for DeskError {
    fn from(error: SocialError) -> Self {
        Self::Social(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Actor {
    Operator(OperatorId),
    /// Desk-driven changes such as rotation advancing.
    System,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuditAction {
    Command(DeskCommand),
    RotationAdvanced(MessageKey),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuditResult {
    Applied,
    /// Unknown operator or insufficient role; nothing changed.
    Denied(DeskError),
    /// The operator was authorized but the command was invalid; nothing changed.
    Failed(DeskError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditEntry {
    /// Strictly increasing from 1, never reused even after eviction.
    pub sequence: u64,
    pub at: Timestamp,
    pub actor: Actor,
    pub action: AuditAction,
    pub result: AuditResult,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeskConfig {
    /// Retained audit entries; the oldest are evicted first.
    pub audit_capacity: usize,
    pub rotation_capacity: usize,
    pub max_operators: usize,
    /// How long a rotation-selected message stays on air.
    pub rotation_dwell: NonZeroU64,
}

impl Default for DeskConfig {
    fn default() -> Self {
        Self {
            audit_capacity: 1024,
            rotation_capacity: 64,
            max_operators: 32,
            rotation_dwell: NonZeroU64::new(8_000).unwrap_or(NonZeroU64::MIN),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeskConfigError {
    ZeroAuditCapacity,
    ZeroRotationCapacity,
    ZeroOperatorCapacity,
}

impl fmt::Display for DeskConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ZeroAuditCapacity => "audit capacity must be non-zero",
            Self::ZeroRotationCapacity => "rotation capacity must be non-zero",
            Self::ZeroOperatorCapacity => "operator capacity must be non-zero",
        })
    }
}

impl Error for DeskConfigError {}

/// Role-gated moderation workflow over a [`SocialAggregator`].
pub struct ModerationDesk {
    social: SocialAggregator,
    config: DeskConfig,
    operators: BTreeMap<OperatorId, Role>,
    hidden: BTreeSet<MessageKey>,
    rotation: Vec<MessageKey>,
    rotation_cursor: usize,
    on_air: Option<OnAir>,
    on_air_generation: u64,
    audit: VecDeque<AuditEntry>,
    next_sequence: u64,
    evicted_audit_entries: u64,
    last_time: Timestamp,
}

impl ModerationDesk {
    /// Creates a desk whose first operator is an admin.
    ///
    /// # Errors
    ///
    /// Returns an error when any configured capacity is zero.
    pub fn new(
        social: SocialAggregator,
        config: DeskConfig,
        initial_admin: impl Into<OperatorId>,
    ) -> Result<Self, DeskConfigError> {
        if config.audit_capacity == 0 {
            return Err(DeskConfigError::ZeroAuditCapacity);
        }
        if config.rotation_capacity == 0 {
            return Err(DeskConfigError::ZeroRotationCapacity);
        }
        if config.max_operators == 0 {
            return Err(DeskConfigError::ZeroOperatorCapacity);
        }
        let mut operators = BTreeMap::new();
        operators.insert(initial_admin.into(), Role::Admin);
        Ok(Self {
            social,
            config,
            operators,
            hidden: BTreeSet::new(),
            rotation: Vec::new(),
            rotation_cursor: 0,
            on_air: None,
            on_air_generation: 0,
            audit: VecDeque::new(),
            next_sequence: 1,
            evicted_audit_entries: 0,
            last_time: Timestamp(0),
        })
    }

    #[must_use]
    pub const fn config(&self) -> &DeskConfig {
        &self.config
    }

    /// Read-only access to the aggregated messages and provider state.
    #[must_use]
    pub const fn social(&self) -> &SocialAggregator {
        &self.social
    }

    #[must_use]
    pub fn role(&self, operator: &OperatorId) -> Option<Role> {
        self.operators.get(operator).copied()
    }

    #[must_use]
    pub const fn operators(&self) -> &BTreeMap<OperatorId, Role> {
        &self.operators
    }

    #[must_use]
    pub fn is_hidden(&self, key: &MessageKey) -> bool {
        self.hidden.contains(key)
    }

    #[must_use]
    pub fn rotation(&self) -> &[MessageKey] {
        &self.rotation
    }

    #[must_use]
    pub const fn on_air(&self) -> Option<&OnAir> {
        self.on_air.as_ref()
    }

    #[must_use]
    pub fn on_air_message(&self) -> Option<&SocialMessage> {
        self.on_air
            .as_ref()
            .and_then(|on_air| self.social.message(&on_air.key))
    }

    /// Increments whenever the on-air message changes; title consumers can
    /// compare it to decide whether to push an update.
    #[must_use]
    pub const fn on_air_generation(&self) -> u64 {
        self.on_air_generation
    }

    /// Retained audit entries, oldest first.
    #[must_use]
    pub fn audit(&self) -> impl ExactSizeIterator<Item = &AuditEntry> {
        self.audit.iter()
    }

    /// Retained audit entries with a sequence greater than `sequence`.
    pub fn audit_after(&self, sequence: u64) -> impl Iterator<Item = &AuditEntry> {
        self.audit
            .iter()
            .filter(move |entry| entry.sequence > sequence)
    }

    #[must_use]
    pub const fn evicted_audit_entries(&self) -> u64 {
        self.evicted_audit_entries
    }

    /// Ingests one message into the moderation queue.
    ///
    /// # Errors
    ///
    /// Returns [`DeskError::ClockRegression`] or the aggregator's queue error.
    pub fn ingest(
        &mut self,
        message: IncomingMessage,
        now: Timestamp,
    ) -> Result<IngestOutcome, DeskError> {
        self.observe(now)?;
        Ok(self.social.ingest(message, now)?)
    }

    /// Polls a provider adapter into the moderation queue.
    ///
    /// # Errors
    ///
    /// Returns the provider error; time regression is reported as a transport error.
    pub fn poll_provider<P: SocialProvider>(
        &mut self,
        provider: &mut P,
        now: Timestamp,
    ) -> Result<PollReport, ProviderError> {
        self.observe(now)
            .map_err(|error| ProviderError::Transport(error.to_string()))?;
        self.social.poll_provider(provider, now)
    }

    /// Expires pending messages whose provider expiry has elapsed.
    ///
    /// # Errors
    ///
    /// Returns [`DeskError::ClockRegression`].
    pub fn expire_due(&mut self, now: Timestamp) -> Result<usize, DeskError> {
        self.observe(now)?;
        Ok(self.social.expire_due(now))
    }

    /// Authorizes, applies, and audits one operator command. Every authorized
    /// or denied attempt is audited; only a clock regression is not.
    ///
    /// # Errors
    ///
    /// Returns why the command was denied or could not be applied. A failed
    /// command changes nothing except the audit log.
    pub fn execute(
        &mut self,
        operator: &OperatorId,
        command: DeskCommand,
        now: Timestamp,
    ) -> Result<(), DeskError> {
        self.observe(now)?;
        let (result, audit_result) = match self.authorize(operator, &command) {
            Err(error) => (Err(error.clone()), AuditResult::Denied(error)),
            Ok(()) => match self.apply(&command, now) {
                Ok(()) => (Ok(()), AuditResult::Applied),
                Err(error) => (Err(error.clone()), AuditResult::Failed(error)),
            },
        };
        self.record(
            now,
            Actor::Operator(operator.clone()),
            AuditAction::Command(command),
            audit_result,
        );
        result
    }

    /// Advances rotation when nothing is on air or the rotation dwell has
    /// elapsed. A manually featured message is never replaced. Returns the
    /// newly selected key, if any.
    ///
    /// # Errors
    ///
    /// Returns [`DeskError::ClockRegression`].
    pub fn tick(&mut self, now: Timestamp) -> Result<Option<MessageKey>, DeskError> {
        self.observe(now)?;
        if self.rotation.is_empty() {
            return Ok(None);
        }
        let due = match &self.on_air {
            None => true,
            Some(on_air) => {
                on_air.source == OnAirSource::Rotation
                    && now.0.saturating_sub(on_air.since.0) >= self.config.rotation_dwell.get()
            }
        };
        if !due {
            return Ok(None);
        }
        let index = self.rotation_cursor % self.rotation.len();
        let key = self.rotation[index].clone();
        self.rotation_cursor = (index + 1) % self.rotation.len();
        self.set_on_air(Some(OnAir {
            key: key.clone(),
            since: now,
            source: OnAirSource::Rotation,
        }));
        self.record(
            now,
            Actor::System,
            AuditAction::RotationAdvanced(key.clone()),
            AuditResult::Applied,
        );
        Ok(Some(key))
    }

    /// Typed field values for the on-air message, or cleared values when off air.
    ///
    /// # Errors
    ///
    /// Returns a mapping error when a source value cannot be represented.
    pub fn on_air_fields(
        &self,
        mapping: &TypedTitleMapping,
    ) -> Result<Vec<(FieldId, FieldValue)>, TitleMappingError> {
        match self.on_air_message() {
            Some(message) => mapping.values(message),
            None => Ok(mapping.cleared_values()),
        }
    }

    /// Writes the on-air fields into a title scene at its current revision.
    ///
    /// # Errors
    ///
    /// Returns a mapping error or the scene's typed update error; on error the
    /// scene is unchanged.
    pub fn apply_on_air_to_scene(
        &self,
        mapping: &TypedTitleMapping,
        scene: &mut TitleScene,
    ) -> Result<u64, TitleSyncError> {
        let updates = self
            .on_air_fields(mapping)
            .map_err(TitleSyncError::Mapping)?;
        scene
            .update_fields(scene.revision(), &updates)
            .map_err(TitleSyncError::Update)
    }

    fn observe(&mut self, now: Timestamp) -> Result<(), DeskError> {
        if now < self.last_time {
            return Err(DeskError::ClockRegression {
                last: self.last_time,
                now,
            });
        }
        self.last_time = now;
        Ok(())
    }

    fn authorize(&self, operator: &OperatorId, command: &DeskCommand) -> Result<(), DeskError> {
        let role = self
            .role(operator)
            .ok_or_else(|| DeskError::UnknownOperator(operator.clone()))?;
        let required = command.required_role();
        if role < required {
            return Err(DeskError::PermissionDenied {
                operator: operator.clone(),
                role,
                required,
            });
        }
        Ok(())
    }

    fn apply(&mut self, command: &DeskCommand, now: Timestamp) -> Result<(), DeskError> {
        match command {
            DeskCommand::Approve(key) => Ok(self.social.approve(key)?),
            DeskCommand::Reject { key, reason } => Ok(self.social.reject(key, reason.clone())?),
            DeskCommand::Block(key) => Ok(self.social.block(key)?),
            DeskCommand::Unblock {
                provider_id,
                account_id,
                author_id,
            } => {
                if self
                    .social
                    .unblock_author(provider_id, account_id, author_id)
                {
                    Ok(())
                } else {
                    Err(DeskError::AuthorNotBlocked)
                }
            }
            DeskCommand::Hide(key) => {
                self.require_visible_approved(key)?;
                self.hidden.insert(key.clone());
                self.remove_rotation_entry(key);
                if self
                    .on_air
                    .as_ref()
                    .is_some_and(|on_air| &on_air.key == key)
                {
                    self.set_on_air(None);
                }
                Ok(())
            }
            DeskCommand::Unhide(key) => {
                if self.hidden.remove(key) {
                    Ok(())
                } else {
                    Err(DeskError::NotHidden(key.clone()))
                }
            }
            DeskCommand::Feature(key) => {
                self.require_visible_approved(key)?;
                self.set_on_air(Some(OnAir {
                    key: key.clone(),
                    since: now,
                    source: OnAirSource::Manual,
                }));
                Ok(())
            }
            DeskCommand::ClearOnAir => {
                self.set_on_air(None);
                Ok(())
            }
            DeskCommand::AddToRotation(key) => {
                self.require_visible_approved(key)?;
                if self.rotation.contains(key) {
                    return Err(DeskError::AlreadyInRotation(key.clone()));
                }
                if self.rotation.len() >= self.config.rotation_capacity {
                    return Err(DeskError::RotationFull {
                        capacity: self.config.rotation_capacity,
                    });
                }
                self.rotation.push(key.clone());
                Ok(())
            }
            DeskCommand::RemoveFromRotation(key) => {
                if self.remove_rotation_entry(key) {
                    Ok(())
                } else {
                    Err(DeskError::NotInRotation(key.clone()))
                }
            }
            DeskCommand::SetRole { operator, role } => {
                match self.operators.get(operator) {
                    None if self.operators.len() >= self.config.max_operators => {
                        return Err(DeskError::OperatorLimit {
                            capacity: self.config.max_operators,
                        });
                    }
                    Some(Role::Admin) if *role != Role::Admin => {
                        self.require_other_admin(operator)?;
                    }
                    Some(_) | None => {}
                }
                self.operators.insert(operator.clone(), *role);
                Ok(())
            }
            DeskCommand::RemoveOperator(operator) => {
                match self.operators.get(operator) {
                    None => return Err(DeskError::UnknownOperator(operator.clone())),
                    Some(Role::Admin) => self.require_other_admin(operator)?,
                    Some(_) => {}
                }
                self.operators.remove(operator);
                Ok(())
            }
        }
    }

    fn require_visible_approved(&self, key: &MessageKey) -> Result<(), DeskError> {
        let message = self
            .social
            .message(key)
            .ok_or_else(|| DeskError::Social(SocialError::MessageNotFound(key.clone())))?;
        if message.moderation != ModerationState::Approved {
            return Err(DeskError::NotApproved(key.clone()));
        }
        if self.hidden.contains(key) {
            return Err(DeskError::Hidden(key.clone()));
        }
        Ok(())
    }

    fn require_other_admin(&self, operator: &OperatorId) -> Result<(), DeskError> {
        let other_admin = self
            .operators
            .iter()
            .any(|(id, role)| id != operator && *role == Role::Admin);
        if other_admin {
            Ok(())
        } else {
            Err(DeskError::LastAdmin(operator.clone()))
        }
    }

    /// Removes a rotation entry while keeping the cursor on the same next entry.
    fn remove_rotation_entry(&mut self, key: &MessageKey) -> bool {
        let Some(index) = self.rotation.iter().position(|entry| entry == key) else {
            return false;
        };
        self.rotation.remove(index);
        if index < self.rotation_cursor {
            self.rotation_cursor -= 1;
        }
        if self.rotation_cursor >= self.rotation.len() {
            self.rotation_cursor = 0;
        }
        true
    }

    fn set_on_air(&mut self, on_air: Option<OnAir>) {
        let changed = match (&self.on_air, &on_air) {
            (None, None) => false,
            (Some(current), Some(next)) => current.key != next.key,
            _ => true,
        };
        self.on_air = on_air;
        if changed {
            self.on_air_generation = self.on_air_generation.wrapping_add(1);
        }
    }

    fn record(&mut self, at: Timestamp, actor: Actor, action: AuditAction, result: AuditResult) {
        if self.audit.len() >= self.config.audit_capacity {
            self.audit.pop_front();
            self.evicted_audit_entries = self.evicted_audit_entries.saturating_add(1);
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.audit.push_back(AuditEntry {
            sequence,
            at,
            actor,
            action,
            result,
        });
    }
}

/// Maps one message source onto one typed title field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypedTitleBinding {
    pub field: FieldId,
    pub source: TitleSourceField,
    /// Text is truncated to this many Unicode scalar values.
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TypedTitleMapping {
    pub bindings: Vec<TypedTitleBinding>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleMappingError {
    /// `CreatedAt` maps to a number field and does not fit in `i64`.
    TimestampOutOfRange { field: FieldId, value: Timestamp },
}

impl fmt::Display for TitleMappingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimestampOutOfRange { field, value } => write!(
                formatter,
                "timestamp {} for field {field} does not fit in a number field",
                value.0
            ),
        }
    }
}

impl Error for TitleMappingError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleSyncError {
    Mapping(TitleMappingError),
    Update(UpdateError),
}

impl fmt::Display for TitleSyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mapping(error) => error.fmt(formatter),
            Self::Update(error) => error.fmt(formatter),
        }
    }
}

impl Error for TitleSyncError {}

impl TypedTitleMapping {
    /// Typed values for a message: `CreatedAt` is a number field and every
    /// other source is a text field.
    ///
    /// # Errors
    ///
    /// Returns [`TitleMappingError::TimestampOutOfRange`] for timestamps above `i64::MAX`.
    pub fn values(
        &self,
        message: &SocialMessage,
    ) -> Result<Vec<(FieldId, FieldValue)>, TitleMappingError> {
        self.bindings
            .iter()
            .map(|binding| {
                let text = match binding.source {
                    TitleSourceField::CreatedAt => {
                        let number = i64::try_from(message.created_at.0).map_err(|_| {
                            TitleMappingError::TimestampOutOfRange {
                                field: binding.field,
                                value: message.created_at,
                            }
                        })?;
                        return Ok((binding.field, FieldValue::Number(number)));
                    }
                    TitleSourceField::AuthorName => message.author_name.as_str(),
                    TitleSourceField::AuthorHandle => {
                        message.author_handle.as_deref().unwrap_or_default()
                    }
                    TitleSourceField::Body => message.body.as_str(),
                    TitleSourceField::ProviderId => message.key.provider_id.as_str(),
                    TitleSourceField::AccountId => message.key.account_id.as_str(),
                    TitleSourceField::MessageId => message.key.message_id.as_str(),
                };
                Ok((
                    binding.field,
                    FieldValue::Text(truncate_chars(text, binding.max_chars)),
                ))
            })
            .collect()
    }

    /// Values that clear every bound field: empty text, or zero for `CreatedAt`.
    #[must_use]
    pub fn cleared_values(&self) -> Vec<(FieldId, FieldValue)> {
        self.bindings
            .iter()
            .map(|binding| {
                let value = match binding.source {
                    TitleSourceField::CreatedAt => FieldValue::Number(0),
                    _ => FieldValue::Text(String::new()),
                };
                (binding.field, value)
            })
            .collect()
    }
}

fn truncate_chars(text: &str, max_chars: Option<usize>) -> String {
    match max_chars.and_then(|max| text.char_indices().nth(max)) {
        Some((end, _)) => text[..end].to_owned(),
        None => text.to_owned(),
    }
}
