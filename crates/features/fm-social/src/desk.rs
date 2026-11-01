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
    AccountId, AuthorId, BlockedAuthor, IncomingMessage, IngestOutcome, MessageKey, ModerationFlag,
    ModerationState, OperatorId, PollReport, ProviderError, ProviderId, SocialAggregator,
    SocialError, SocialMessage, SocialProvider, Timestamp, TitleSourceField,
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

    /// Every hidden message key, ascending.
    #[must_use]
    pub const fn hidden(&self) -> &BTreeSet<MessageKey> {
        &self.hidden
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

/// Why a moderation-desk document could not be restored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeskDocumentError {
    /// The first line is not the current document header.
    Header,
    /// The required config line is missing, has the wrong fields, or has a
    /// non-canonical value.
    Config,
    /// An operator line is missing, malformed, or has an invalid role.
    Operator,
    /// Operator IDs are not in strictly ascending order.
    Order,
    /// Two operator lines name the same ID.
    DuplicateOperator,
    /// More operators than the configured maximum.
    TooManyOperators,
    /// No operator is an admin.
    NoAdmin,
    /// A configured capacity is zero.
    ZeroCapacity,
    /// The rotation dwell is zero.
    ZeroDwell,
    /// A state line is missing, out of order, duplicated, or malformed.
    State,
    /// The next audit sequence is zero.
    Sequence,
    /// The rotation cursor does not name an entry of the rotation.
    RotationCursor,
    /// Two hidden or rotation lines name the same message.
    DuplicateKey,
    /// Hidden message lines are not in strictly ascending key order.
    HiddenOrder,
    /// More hidden messages than the document limit.
    TooManyHidden,
    /// More rotation entries than the configured capacity.
    TooManyRotation,
    /// The on-air line names an invalid state.
    OnAir,
    /// An audit line is malformed, out of sequence, or too large.
    Audit,
    /// A message or blocked-author line is malformed, duplicated, out of
    /// bounds, or names a message the desk state does not match.
    Message,
}

impl fmt::Display for DeskDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Header => "moderation desk document header is missing or unknown",
            Self::Config => "moderation desk document config line is malformed",
            Self::Operator => "moderation desk document operator line is malformed",
            Self::Order => "moderation desk operators are not in ascending ID order",
            Self::DuplicateOperator => "moderation desk document repeats an operator",
            Self::TooManyOperators => "moderation desk document has too many operators",
            Self::NoAdmin => "moderation desk document has no admin",
            Self::ZeroCapacity => "moderation desk capacities must be non-zero",
            Self::ZeroDwell => "moderation desk rotation dwell must be non-zero",
            Self::State => "moderation desk document state line is malformed or out of order",
            Self::Sequence => "moderation desk document audit sequence must be non-zero",
            Self::RotationCursor => "moderation desk document rotation cursor is out of range",
            Self::DuplicateKey => "moderation desk document repeats a message key",
            Self::HiddenOrder => "moderation desk document hidden keys are not in ascending order",
            Self::TooManyHidden => "moderation desk document has too many hidden keys",
            Self::TooManyRotation => "moderation desk document rotation exceeds its capacity",
            Self::OnAir => "moderation desk document on-air state is invalid",
            Self::Audit => "moderation desk document audit line is malformed or out of sequence",
            Self::Message => "moderation desk document message state is malformed or inconsistent",
        })
    }
}

impl Error for DeskDocumentError {}

impl Role {
    /// The document token of the role.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Moderator => "moderator",
            Self::Producer => "producer",
            Self::Admin => "admin",
        }
    }

    /// The role named by `token`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "viewer" => Self::Viewer,
            "moderator" => Self::Moderator,
            "producer" => Self::Producer,
            "admin" => Self::Admin,
            _ => return None,
        })
    }
}

impl ModerationDesk {
    /// Encodes the desk's config, operators, messages, hidden, rotation,
    /// on-air, and audit state as canonical text:
    ///
    /// ```text
    /// freemix-social-desk 4
    /// config <audit> <rotation> <max operators> <dwell ms>
    /// operator <id hex> <role>
    /// sequence <next audit sequence>
    /// rotation-cursor <index>
    /// evicted <audit entries evicted>
    /// last <observed ms>
    /// hidden <provider hex> <account hex> <message hex>
    /// rotation <provider hex> <account hex> <message hex>
    /// on-air <provider hex> <account hex> <message hex> <since ms> <manual|rotation> <generation>
    /// audit <seq> <at ms> <actor> <action> <result>
    /// message <provider> <account> <message> <author> <name> <handle|none> <body> <created> <ingested> <expires|none> <moderation> <flags>...
    /// blocked-author <provider> <account> <author>
    /// ```
    ///
    /// Hidden keys ascend, rotation keys keep their rotation order, the on-air
    /// line is present only while a message is on air, audit entries follow in
    /// sequence order, messages keep their ingestion order, and blocked
    /// authors ascend.
    #[must_use]
    pub fn to_document(&self) -> String {
        use std::fmt::Write as _;
        let mut output = String::from("freemix-social-desk 4\n");
        writeln!(
            output,
            "config {} {} {} {}",
            self.config.audit_capacity,
            self.config.rotation_capacity,
            self.config.max_operators,
            self.config.rotation_dwell.get()
        )
        .expect("writing to a string cannot fail");
        for (operator, role) in &self.operators {
            writeln!(
                output,
                "operator {} {}",
                hex_text(operator.as_str()),
                role.token()
            )
            .expect("writing to a string cannot fail");
        }
        writeln!(output, "sequence {}", self.next_sequence)
            .expect("writing to a string cannot fail");
        writeln!(output, "rotation-cursor {}", self.rotation_cursor)
            .expect("writing to a string cannot fail");
        writeln!(output, "evicted {}", self.evicted_audit_entries)
            .expect("writing to a string cannot fail");
        writeln!(output, "last {}", self.last_time.0).expect("writing to a string cannot fail");
        for key in &self.hidden {
            writeln!(output, "hidden {}", key_document(key))
                .expect("writing to a string cannot fail");
        }
        for key in &self.rotation {
            writeln!(output, "rotation {}", key_document(key))
                .expect("writing to a string cannot fail");
        }
        if let Some(on_air) = &self.on_air {
            writeln!(
                output,
                "on-air {} {} {} {}",
                key_document(&on_air.key),
                on_air.since.0,
                on_air.source.token(),
                self.on_air_generation
            )
            .expect("writing to a string cannot fail");
        }
        for entry in &self.audit {
            writeln!(output, "audit {}", audit_entry_document(entry))
                .expect("writing to a string cannot fail");
        }
        for key in &self.social.message_order {
            if let Some(message) = self.social.messages.get(key) {
                writeln!(output, "message {}", message_document(message))
                    .expect("writing to a string cannot fail");
            }
        }
        let mut blocked = self
            .social
            .blocked_authors
            .iter()
            .map(|author| {
                (
                    author.provider.as_str(),
                    author.account.as_str(),
                    author.author.as_str(),
                )
            })
            .collect::<Vec<_>>();
        blocked.sort_unstable();
        for (provider, account, author) in blocked {
            writeln!(
                output,
                "blocked-author {} {} {}",
                identity_document(provider),
                identity_document(account),
                identity_document(author)
            )
            .expect("writing to a string cannot fail");
        }
        output
    }

    /// Restores a desk from `document` over `social`. Hidden, rotation,
    /// rotation cursor, on-air, the observed clock, and the audit counters
    /// survive; the audit log itself starts empty.
    ///
    /// # Errors
    ///
    /// Returns [`DeskDocumentError`] for anything but the exact contract.
    pub fn from_document(
        document: &str,
        social: SocialAggregator,
    ) -> Result<Self, DeskDocumentError> {
        let mut lines = document.lines();
        if lines.next() != Some("freemix-social-desk 4") {
            return Err(DeskDocumentError::Header);
        }
        let Some(config_line) = lines.next() else {
            return Err(DeskDocumentError::Config);
        };
        let parts: Vec<&str> = config_line.split(' ').collect();
        let ["config", audit, rotation, operators, dwell] = parts.as_slice() else {
            return Err(DeskDocumentError::Config);
        };
        let capacity = |value: &str| {
            value
                .parse::<usize>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or(DeskDocumentError::ZeroCapacity)
        };
        let dwell = dwell
            .parse::<u64>()
            .ok()
            .and_then(NonZeroU64::new)
            .ok_or(DeskDocumentError::ZeroDwell)?;
        let config = DeskConfig {
            audit_capacity: capacity(audit)?,
            rotation_capacity: capacity(rotation)?,
            max_operators: capacity(operators)?,
            rotation_dwell: dwell,
        };
        let mut defined = BTreeMap::new();
        let mut previous: Option<String> = None;
        let mut state_lines = Vec::new();
        for line in lines {
            if let Some(rest) = line.strip_prefix("operator ") {
                if !state_lines.is_empty() {
                    return Err(DeskDocumentError::Operator);
                }
                let parts: Vec<&str> = rest.split(' ').collect();
                let [id, role] = parts.as_slice() else {
                    return Err(DeskDocumentError::Operator);
                };
                let id = unhex_text(id).ok_or(DeskDocumentError::Operator)?;
                if let Some(previous) = &previous {
                    if id == *previous {
                        return Err(DeskDocumentError::DuplicateOperator);
                    }
                    if id < *previous {
                        return Err(DeskDocumentError::Order);
                    }
                }
                previous = Some(id.clone());
                let role = Role::from_token(role).ok_or(DeskDocumentError::Operator)?;
                defined.insert(OperatorId::new(id), role);
            } else {
                state_lines.push(line);
            }
        }
        if defined.is_empty() {
            return Err(DeskDocumentError::Operator);
        }
        if defined.len() > config.max_operators {
            return Err(DeskDocumentError::TooManyOperators);
        }
        if !defined.values().any(|role| *role == Role::Admin) {
            return Err(DeskDocumentError::NoAdmin);
        }

        let mut state = state_lines.into_iter().peekable();
        let next_sequence = state_u64(&mut state, "sequence")?;
        if next_sequence == 0 {
            return Err(DeskDocumentError::Sequence);
        }
        let rotation_cursor = state_usize(&mut state, "rotation-cursor")?;
        let evicted_audit_entries = state_u64(&mut state, "evicted")?;
        let last_time = Timestamp(state_u64(&mut state, "last")?);

        let mut hidden = BTreeSet::new();
        let mut previous: Option<MessageKey> = None;
        while state.peek().is_some_and(|line| line.starts_with("hidden ")) {
            let line = state.next().unwrap_or_default();
            let key = key_from_line(line, "hidden")?;
            if let Some(previous) = &previous {
                if key == *previous {
                    return Err(DeskDocumentError::DuplicateKey);
                }
                if key < *previous {
                    return Err(DeskDocumentError::HiddenOrder);
                }
            }
            previous = Some(key.clone());
            hidden.insert(key);
            if hidden.len() > DESK_DOCUMENT_MAX_HIDDEN {
                return Err(DeskDocumentError::TooManyHidden);
            }
        }

        let mut rotation = Vec::new();
        let mut rotation_keys = BTreeSet::new();
        while state
            .peek()
            .is_some_and(|line| line.starts_with("rotation "))
        {
            let line = state.next().unwrap_or_default();
            let key = key_from_line(line, "rotation")?;
            if hidden.contains(&key) {
                return Err(DeskDocumentError::OnAir);
            }
            if !rotation_keys.insert(key.clone()) {
                return Err(DeskDocumentError::DuplicateKey);
            }
            rotation.push(key);
            if rotation.len() > config.rotation_capacity {
                return Err(DeskDocumentError::TooManyRotation);
            }
        }
        if rotation.is_empty() {
            if rotation_cursor != 0 {
                return Err(DeskDocumentError::RotationCursor);
            }
        } else if rotation_cursor >= rotation.len() {
            return Err(DeskDocumentError::RotationCursor);
        }

        let mut on_air = None;
        let mut on_air_generation = 0;
        if state.peek().is_some_and(|line| line.starts_with("on-air ")) {
            let line = state.next().unwrap_or_default();
            let Some(rest) = line.strip_prefix("on-air ") else {
                return Err(DeskDocumentError::State);
            };
            let parts: Vec<&str> = rest.split(' ').collect();
            let [provider, account, message, since, source, generation] = parts.as_slice() else {
                return Err(DeskDocumentError::OnAir);
            };
            let key = key_from_parts(provider, account, message)?;
            let since = since
                .parse::<u64>()
                .map(Timestamp)
                .map_err(|_| DeskDocumentError::OnAir)?;
            if since > last_time {
                return Err(DeskDocumentError::OnAir);
            }
            let source = OnAirSource::from_token(source).ok_or(DeskDocumentError::OnAir)?;
            let generation = generation
                .parse::<u64>()
                .ok()
                .filter(|generation| *generation > 0)
                .ok_or(DeskDocumentError::OnAir)?;
            if hidden.contains(&key) {
                return Err(DeskDocumentError::OnAir);
            }
            if source == OnAirSource::Rotation && !rotation_keys.contains(&key) {
                return Err(DeskDocumentError::OnAir);
            }
            on_air = Some(OnAir { key, since, source });
            on_air_generation = generation;
        }
        let mut audit = VecDeque::new();
        let mut previous_sequence = 0;
        let mut previous_at = Timestamp(0);
        while state.peek().is_some_and(|line| line.starts_with("audit ")) {
            let line = state.next().unwrap_or_default();
            let entry = audit_entry_from_line(line)?;
            if entry.sequence == 0
                || entry.sequence >= next_sequence
                || entry.sequence <= previous_sequence
                || entry.at < previous_at
                || entry.at > last_time
                || audit.len() >= config.audit_capacity
            {
                return Err(DeskDocumentError::Audit);
            }
            previous_sequence = entry.sequence;
            previous_at = entry.at;
            audit.push_back(entry);
        }
        let mut messages = std::collections::HashMap::new();
        let mut message_order = Vec::new();
        let mut pending = VecDeque::new();
        while state
            .peek()
            .is_some_and(|line| line.starts_with("message "))
        {
            let line = state.next().unwrap_or_default();
            let message = message_from_line(line)?;
            if messages.contains_key(&message.key) {
                return Err(DeskDocumentError::DuplicateKey);
            }
            if message.ingested_at > last_time {
                return Err(DeskDocumentError::Message);
            }
            if message.moderation == ModerationState::Pending {
                if pending.len() >= social.queue_capacity {
                    return Err(DeskDocumentError::Message);
                }
                pending.push_back(message.key.clone());
            }
            message_order.push(message.key.clone());
            messages.insert(message.key.clone(), message);
            if messages.len() > DESK_DOCUMENT_MAX_MESSAGES {
                return Err(DeskDocumentError::Message);
            }
        }
        let mut blocked_authors = std::collections::HashSet::new();
        while state
            .peek()
            .is_some_and(|line| line.starts_with("blocked-author "))
        {
            let line = state.next().unwrap_or_default();
            let author = blocked_author_from_line(line)?;
            if !blocked_authors.insert(author) {
                return Err(DeskDocumentError::Message);
            }
        }
        if state.next().is_some() {
            return Err(DeskDocumentError::State);
        }
        for key in &hidden {
            let message = messages.get(key).ok_or(DeskDocumentError::Message)?;
            if message.moderation != ModerationState::Approved {
                return Err(DeskDocumentError::Message);
            }
        }
        for key in &rotation {
            let message = messages.get(key).ok_or(DeskDocumentError::Message)?;
            if message.moderation != ModerationState::Approved {
                return Err(DeskDocumentError::Message);
            }
        }
        if let Some(on_air) = &on_air {
            let message = messages
                .get(&on_air.key)
                .ok_or(DeskDocumentError::Message)?;
            if message.moderation != ModerationState::Approved {
                return Err(DeskDocumentError::Message);
            }
        }

        let mut social = social;
        social.messages = messages;
        social.message_order = message_order;
        social.pending = pending;
        social.blocked_authors = blocked_authors;
        Ok(Self {
            social,
            config,
            operators: defined,
            hidden,
            rotation,
            rotation_cursor,
            on_air,
            on_air_generation,
            audit,
            next_sequence,
            evicted_audit_entries,
            last_time,
        })
    }
}

/// Most hidden message keys one desk document may hold.
const DESK_DOCUMENT_MAX_HIDDEN: usize = 65_536;
/// Most aggregated messages one desk document may hold.
const DESK_DOCUMENT_MAX_MESSAGES: usize = 65_536;
/// Most moderation flags one restored message may carry.
const DESK_DOCUMENT_MAX_FLAGS: usize = 256;

fn state_line<'a>(
    state: &mut std::iter::Peekable<std::vec::IntoIter<&'a str>>,
    name: &str,
) -> Result<&'a str, DeskDocumentError> {
    let line = state.next().ok_or(DeskDocumentError::State)?;
    line.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix(' '))
        .ok_or(DeskDocumentError::State)
}

fn state_u64(
    state: &mut std::iter::Peekable<std::vec::IntoIter<&str>>,
    name: &str,
) -> Result<u64, DeskDocumentError> {
    state_line(state, name)?
        .parse()
        .map_err(|_| DeskDocumentError::State)
}

fn state_usize(
    state: &mut std::iter::Peekable<std::vec::IntoIter<&str>>,
    name: &str,
) -> Result<usize, DeskDocumentError> {
    state_line(state, name)?
        .parse()
        .map_err(|_| DeskDocumentError::State)
}

fn key_from_line(line: &str, name: &str) -> Result<MessageKey, DeskDocumentError> {
    let rest = line
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix(' '))
        .ok_or(DeskDocumentError::State)?;
    let parts: Vec<&str> = rest.split(' ').collect();
    let [provider, account, message] = parts.as_slice() else {
        return Err(DeskDocumentError::State);
    };
    key_from_parts(provider, account, message)
}

fn key_from_parts(
    provider: &str,
    account: &str,
    message: &str,
) -> Result<MessageKey, DeskDocumentError> {
    Ok(MessageKey::new(
        identity_text(provider).ok_or(DeskDocumentError::State)?,
        identity_text(account).ok_or(DeskDocumentError::State)?,
        identity_text(message).ok_or(DeskDocumentError::State)?,
    ))
}

fn key_document(key: &MessageKey) -> String {
    format!(
        "{} {} {}",
        identity_document(key.provider_id.as_str()),
        identity_document(key.account_id.as_str()),
        identity_document(key.message_id.as_str())
    )
}

fn identity_document(text: &str) -> String {
    if text.is_empty() {
        "-".to_owned()
    } else {
        hex_text(text)
    }
}

fn identity_text(text: &str) -> Option<String> {
    if text == "-" {
        Some(String::new())
    } else {
        unhex_text(text)
    }
}

fn audit_entry_document(entry: &AuditEntry) -> String {
    let mut parts = Vec::new();
    parts.push(entry.sequence.to_string());
    parts.push(entry.at.0.to_string());
    match &entry.actor {
        Actor::System => parts.push("system".to_owned()),
        Actor::Operator(operator) => {
            parts.push("operator".to_owned());
            parts.push(identity_document(operator.as_str()));
        }
    }
    match &entry.action {
        AuditAction::RotationAdvanced(key) => {
            parts.push("rotation-advanced".to_owned());
            push_key(&mut parts, key);
        }
        AuditAction::Command(command) => {
            parts.push("command".to_owned());
            push_command(&mut parts, command);
        }
    }
    match &entry.result {
        AuditResult::Applied => parts.push("applied".to_owned()),
        AuditResult::Denied(error) => {
            parts.push("denied".to_owned());
            push_desk_error(&mut parts, error);
        }
        AuditResult::Failed(error) => {
            parts.push("failed".to_owned());
            push_desk_error(&mut parts, error);
        }
    }
    parts.join(" ")
}

fn message_document(message: &SocialMessage) -> String {
    let mut parts = Vec::new();
    push_key(&mut parts, &message.key);
    parts.push(identity_document(message.author_id.as_str()));
    parts.push(identity_document(&message.author_name));
    parts.push(match &message.author_handle {
        Some(handle) => identity_document(handle),
        None => "none".to_owned(),
    });
    parts.push(identity_document(&message.body));
    parts.push(message.created_at.0.to_string());
    parts.push(message.ingested_at.0.to_string());
    parts.push(
        message
            .expires_at
            .map_or_else(|| "none".to_owned(), |expires| expires.0.to_string()),
    );
    match &message.moderation {
        ModerationState::Pending => parts.push("pending".to_owned()),
        ModerationState::Approved => parts.push("approved".to_owned()),
        ModerationState::Rejected { reason } => {
            parts.push("rejected".to_owned());
            parts.push(identity_document(reason));
        }
        ModerationState::Blocked => parts.push("blocked".to_owned()),
        ModerationState::Expired => parts.push("expired".to_owned()),
    }
    parts.push(message.flags.len().to_string());
    for flag in &message.flags {
        match flag {
            ModerationFlag::Profanity(text) => {
                parts.push("profanity".to_owned());
                parts.push(identity_document(text));
            }
            ModerationFlag::Redacted(text) => {
                parts.push("redacted".to_owned());
                parts.push(identity_document(text));
            }
        }
    }
    parts.join(" ")
}

fn push_key(parts: &mut Vec<String>, key: &MessageKey) {
    parts.push(identity_document(key.provider_id.as_str()));
    parts.push(identity_document(key.account_id.as_str()));
    parts.push(identity_document(key.message_id.as_str()));
}

fn push_command(parts: &mut Vec<String>, command: &DeskCommand) {
    match command {
        DeskCommand::Approve(key) => {
            parts.push("approve".to_owned());
            push_key(parts, key);
        }
        DeskCommand::Reject { key, reason } => {
            parts.push("reject".to_owned());
            push_key(parts, key);
            parts.push(identity_document(reason));
        }
        DeskCommand::Block(key) => {
            parts.push("block".to_owned());
            push_key(parts, key);
        }
        DeskCommand::Unblock {
            provider_id,
            account_id,
            author_id,
        } => {
            parts.push("unblock".to_owned());
            parts.push(identity_document(provider_id.as_str()));
            parts.push(identity_document(account_id.as_str()));
            parts.push(identity_document(author_id.as_str()));
        }
        DeskCommand::Hide(key) => {
            parts.push("hide".to_owned());
            push_key(parts, key);
        }
        DeskCommand::Unhide(key) => {
            parts.push("unhide".to_owned());
            push_key(parts, key);
        }
        DeskCommand::Feature(key) => {
            parts.push("feature".to_owned());
            push_key(parts, key);
        }
        DeskCommand::ClearOnAir => parts.push("clear-on-air".to_owned()),
        DeskCommand::AddToRotation(key) => {
            parts.push("add-rotation".to_owned());
            push_key(parts, key);
        }
        DeskCommand::RemoveFromRotation(key) => {
            parts.push("remove-rotation".to_owned());
            push_key(parts, key);
        }
        DeskCommand::SetRole { operator, role } => {
            parts.push("set-role".to_owned());
            parts.push(identity_document(operator.as_str()));
            parts.push(role.token().to_owned());
        }
        DeskCommand::RemoveOperator(operator) => {
            parts.push("remove-operator".to_owned());
            parts.push(identity_document(operator.as_str()));
        }
    }
}

fn push_desk_error(parts: &mut Vec<String>, error: &DeskError) {
    match error {
        DeskError::UnknownOperator(operator) => {
            parts.push("unknown-operator".to_owned());
            parts.push(identity_document(operator.as_str()));
        }
        DeskError::PermissionDenied {
            operator,
            role,
            required,
        } => {
            parts.push("permission-denied".to_owned());
            parts.push(identity_document(operator.as_str()));
            parts.push(role.token().to_owned());
            parts.push(required.token().to_owned());
        }
        DeskError::Social(error) => {
            parts.push("social".to_owned());
            push_social_error(parts, error);
        }
        DeskError::NotApproved(key) => {
            parts.push("not-approved".to_owned());
            push_key(parts, key);
        }
        DeskError::Hidden(key) => {
            parts.push("hidden".to_owned());
            push_key(parts, key);
        }
        DeskError::NotHidden(key) => {
            parts.push("not-hidden".to_owned());
            push_key(parts, key);
        }
        DeskError::AlreadyInRotation(key) => {
            parts.push("already-in-rotation".to_owned());
            push_key(parts, key);
        }
        DeskError::NotInRotation(key) => {
            parts.push("not-in-rotation".to_owned());
            push_key(parts, key);
        }
        DeskError::RotationFull { capacity } => {
            parts.push("rotation-full".to_owned());
            parts.push(capacity.to_string());
        }
        DeskError::AuthorNotBlocked => parts.push("author-not-blocked".to_owned()),
        DeskError::OperatorLimit { capacity } => {
            parts.push("operator-limit".to_owned());
            parts.push(capacity.to_string());
        }
        DeskError::LastAdmin(operator) => {
            parts.push("last-admin".to_owned());
            parts.push(identity_document(operator.as_str()));
        }
        DeskError::ClockRegression { last, now } => {
            parts.push("clock-regression".to_owned());
            parts.push(last.0.to_string());
            parts.push(now.0.to_string());
        }
    }
}

fn push_social_error(parts: &mut Vec<String>, error: &SocialError) {
    match error {
        SocialError::QueueFull { capacity } => {
            parts.push("queue-full".to_owned());
            parts.push(capacity.to_string());
        }
        SocialError::MessageNotFound(key) => {
            parts.push("message-not-found".to_owned());
            push_key(parts, key);
        }
        SocialError::MessageNotPending(key) => {
            parts.push("message-not-pending".to_owned());
            push_key(parts, key);
        }
    }
}

struct FieldTokens<'a> {
    parts: std::vec::IntoIter<&'a str>,
}

impl<'a> FieldTokens<'a> {
    fn new(rest: &'a str) -> Self {
        Self {
            parts: rest.split(' ').collect::<Vec<_>>().into_iter(),
        }
    }

    fn take(&mut self) -> Result<&'a str, DeskDocumentError> {
        self.parts.next().ok_or(DeskDocumentError::Audit)
    }

    fn identity(&mut self) -> Result<String, DeskDocumentError> {
        identity_text(self.take()?).ok_or(DeskDocumentError::Audit)
    }

    fn u64(&mut self) -> Result<u64, DeskDocumentError> {
        self.take()?.parse().map_err(|_| DeskDocumentError::Audit)
    }

    fn usize(&mut self) -> Result<usize, DeskDocumentError> {
        self.take()?.parse().map_err(|_| DeskDocumentError::Audit)
    }

    fn key(&mut self) -> Result<MessageKey, DeskDocumentError> {
        key_from_parts(self.take()?, self.take()?, self.take()?)
            .map_err(|_| DeskDocumentError::Audit)
    }

    fn role(&mut self) -> Result<Role, DeskDocumentError> {
        Role::from_token(self.take()?).ok_or(DeskDocumentError::Audit)
    }

    fn command(&mut self) -> Result<DeskCommand, DeskDocumentError> {
        Ok(match self.take()? {
            "approve" => DeskCommand::Approve(self.key()?),
            "reject" => {
                let key = self.key()?;
                let reason = self.identity()?;
                DeskCommand::Reject { key, reason }
            }
            "block" => DeskCommand::Block(self.key()?),
            "unblock" => DeskCommand::Unblock {
                provider_id: ProviderId::new(self.identity()?),
                account_id: AccountId::new(self.identity()?),
                author_id: AuthorId::new(self.identity()?),
            },
            "hide" => DeskCommand::Hide(self.key()?),
            "unhide" => DeskCommand::Unhide(self.key()?),
            "feature" => DeskCommand::Feature(self.key()?),
            "clear-on-air" => DeskCommand::ClearOnAir,
            "add-rotation" => DeskCommand::AddToRotation(self.key()?),
            "remove-rotation" => DeskCommand::RemoveFromRotation(self.key()?),
            "set-role" => DeskCommand::SetRole {
                operator: OperatorId::new(self.identity()?),
                role: self.role()?,
            },
            "remove-operator" => DeskCommand::RemoveOperator(OperatorId::new(self.identity()?)),
            _ => return Err(DeskDocumentError::Audit),
        })
    }

    fn social_error(&mut self) -> Result<SocialError, DeskDocumentError> {
        Ok(match self.take()? {
            "queue-full" => SocialError::QueueFull {
                capacity: self.usize()?,
            },
            "message-not-found" => SocialError::MessageNotFound(self.key()?),
            "message-not-pending" => SocialError::MessageNotPending(self.key()?),
            _ => return Err(DeskDocumentError::Audit),
        })
    }

    fn desk_error(&mut self) -> Result<DeskError, DeskDocumentError> {
        Ok(match self.take()? {
            "unknown-operator" => DeskError::UnknownOperator(OperatorId::new(self.identity()?)),
            "permission-denied" => DeskError::PermissionDenied {
                operator: OperatorId::new(self.identity()?),
                role: self.role()?,
                required: self.role()?,
            },
            "social" => DeskError::Social(self.social_error()?),
            "not-approved" => DeskError::NotApproved(self.key()?),
            "hidden" => DeskError::Hidden(self.key()?),
            "not-hidden" => DeskError::NotHidden(self.key()?),
            "already-in-rotation" => DeskError::AlreadyInRotation(self.key()?),
            "not-in-rotation" => DeskError::NotInRotation(self.key()?),
            "rotation-full" => DeskError::RotationFull {
                capacity: self.usize()?,
            },
            "author-not-blocked" => DeskError::AuthorNotBlocked,
            "operator-limit" => DeskError::OperatorLimit {
                capacity: self.usize()?,
            },
            "last-admin" => DeskError::LastAdmin(OperatorId::new(self.identity()?)),
            "clock-regression" => DeskError::ClockRegression {
                last: Timestamp(self.u64()?),
                now: Timestamp(self.u64()?),
            },
            _ => return Err(DeskDocumentError::Audit),
        })
    }

    fn done(mut self) -> Result<(), DeskDocumentError> {
        if self.parts.next().is_some() {
            Err(DeskDocumentError::Audit)
        } else {
            Ok(())
        }
    }
}

fn audit_entry_from_line(line: &str) -> Result<AuditEntry, DeskDocumentError> {
    let rest = line
        .strip_prefix("audit ")
        .ok_or(DeskDocumentError::Audit)?;
    let mut tokens = FieldTokens::new(rest);
    let sequence = tokens.u64()?;
    let at = Timestamp(tokens.u64()?);
    let actor = match tokens.take()? {
        "system" => Actor::System,
        "operator" => Actor::Operator(OperatorId::new(tokens.identity()?)),
        _ => return Err(DeskDocumentError::Audit),
    };
    let action = match tokens.take()? {
        "rotation-advanced" => AuditAction::RotationAdvanced(tokens.key()?),
        "command" => AuditAction::Command(tokens.command()?),
        _ => return Err(DeskDocumentError::Audit),
    };
    let result = match tokens.take()? {
        "applied" => AuditResult::Applied,
        "denied" => AuditResult::Denied(tokens.desk_error()?),
        "failed" => AuditResult::Failed(tokens.desk_error()?),
        _ => return Err(DeskDocumentError::Audit),
    };
    tokens.done()?;
    Ok(AuditEntry {
        sequence,
        at,
        actor,
        action,
        result,
    })
}

fn message_from_line(line: &str) -> Result<SocialMessage, DeskDocumentError> {
    message_from_tokens(line).map_err(|_| DeskDocumentError::Message)
}

fn message_from_tokens(line: &str) -> Result<SocialMessage, DeskDocumentError> {
    let rest = line
        .strip_prefix("message ")
        .ok_or(DeskDocumentError::Message)?;
    let mut tokens = FieldTokens::new(rest);
    let key = tokens.key()?;
    let author_id = AuthorId::new(tokens.identity()?);
    let author_name = tokens.identity()?;
    let author_handle = match tokens.take()? {
        "none" => None,
        token => Some(identity_text(token).ok_or(DeskDocumentError::Message)?),
    };
    let body = tokens.identity()?;
    let created_at = Timestamp(tokens.u64()?);
    let ingested_at = Timestamp(tokens.u64()?);
    let expires_at = match tokens.take()? {
        "none" => None,
        token => Some(Timestamp(
            token.parse().map_err(|_| DeskDocumentError::Message)?,
        )),
    };
    let moderation = match tokens.take()? {
        "pending" => ModerationState::Pending,
        "approved" => ModerationState::Approved,
        "rejected" => ModerationState::Rejected {
            reason: tokens.identity()?,
        },
        "blocked" => ModerationState::Blocked,
        "expired" => ModerationState::Expired,
        _ => return Err(DeskDocumentError::Message),
    };
    let flag_count = tokens.usize()?;
    if flag_count > DESK_DOCUMENT_MAX_FLAGS {
        return Err(DeskDocumentError::Message);
    }
    let mut flags = Vec::with_capacity(flag_count);
    for _ in 0..flag_count {
        match tokens.take()? {
            "profanity" => flags.push(ModerationFlag::Profanity(tokens.identity()?)),
            "redacted" => flags.push(ModerationFlag::Redacted(tokens.identity()?)),
            _ => return Err(DeskDocumentError::Message),
        }
    }
    tokens.done()?;
    Ok(SocialMessage {
        key,
        author_id,
        author_name,
        author_handle,
        body,
        created_at,
        ingested_at,
        expires_at,
        moderation,
        flags,
    })
}

fn blocked_author_from_line(line: &str) -> Result<BlockedAuthor, DeskDocumentError> {
    blocked_author_from_tokens(line).map_err(|_| DeskDocumentError::Message)
}

fn blocked_author_from_tokens(line: &str) -> Result<BlockedAuthor, DeskDocumentError> {
    let rest = line
        .strip_prefix("blocked-author ")
        .ok_or(DeskDocumentError::Message)?;
    let parts: Vec<&str> = rest.split(' ').collect();
    let [provider, account, author] = parts.as_slice() else {
        return Err(DeskDocumentError::Message);
    };
    Ok(BlockedAuthor {
        provider: ProviderId::new(identity_text(provider).ok_or(DeskDocumentError::Message)?),
        account: AccountId::new(identity_text(account).ok_or(DeskDocumentError::Message)?),
        author: AuthorId::new(identity_text(author).ok_or(DeskDocumentError::Message)?),
    })
}

impl OnAirSource {
    /// The document token of the source.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Rotation => "rotation",
        }
    }

    /// The source named by `token`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "manual" => Self::Manual,
            "rotation" => Self::Rotation,
            _ => return None,
        })
    }
}

fn hex_text(text: &str) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(text.len() * 2);
    for byte in text.bytes() {
        write!(output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

fn unhex_text(text: &str) -> Option<String> {
    if text.is_empty() || text.len() % 2 != 0 {
        return None;
    }
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        output.push((digit(pair[0])? << 4) | digit(pair[1])?);
    }
    String::from_utf8(output).ok()
}

#[cfg(test)]
mod document_tests {
    use super::*;
    use crate::{ContentPolicy, ProfanityPolicy};

    fn social() -> SocialAggregator {
        SocialAggregator::new(
            8,
            ContentPolicy {
                profanity: ProfanityPolicy::default(),
                redactions: Vec::new(),
            },
        )
    }

    #[test]
    fn desk_documents_round_trip_config_operators_state_and_audit() {
        let document = "freemix-social-desk 4\n\
                        config 512 16 4 4000\n\
                        operator 616c696365 admin\n\
                        operator 626f62 moderator\n\
                        sequence 12\n\
                        rotation-cursor 1\n\
                        evicted 3\n\
                        last 9000\n\
                        hidden 70726f76 61636374 6d31\n\
                        rotation 70726f76 61636374 6d32\n\
                        rotation 70726f76 61636374 6d33\n\
                        on-air 70726f76 61636374 6d32 8000 rotation 5\n\
                        audit 6 7000 operator 616c696365 command feature 70726f76 61636374 6d32 applied\n\
                        audit 7 8000 system rotation-advanced 70726f76 61636374 6d32 applied\n\
                        audit 8 8500 operator 626f62 command approve 70726f76 61636374 6d34 denied permission-denied 626f62 moderator producer\n\
                        audit 9 8700 operator 616c696365 command hide 70726f76 61636374 6d33 failed hidden 70726f76 61636374 6d33\n\
                        audit 10 8900 operator 616c696365 command unblock 70726f76 61636374 617574686f72 failed author-not-blocked\n\
                        audit 11 9000 operator 616c696365 command reject 70726f76 61636374 6d34 7370616d applied\n\
                        message 70726f76 61636374 6d31 616c696365 416c696365 none 68656c6c6f 1000 2000 none approved 0\n\
                        message 70726f76 61636374 6d32 626f62 426f62 40626f62 74776f 1500 2500 none approved 0\n\
                        message 70726f76 61636374 6d33 6361726f6c 4361726f6c none 7468726565 1600 2600 12000 approved 0\n\
                        message 70726f76 61636374 6d34 64617665 44617665 none 666f7572 1700 2700 none rejected 7370616d 1 profanity 7370616d\n\
                        blocked-author 70726f76 61636374 626f62\n";
        let desk = ModerationDesk::from_document(document, social()).unwrap();
        assert_eq!(desk.config().audit_capacity, 512);
        assert_eq!(desk.config().rotation_capacity, 16);
        assert_eq!(desk.config().max_operators, 4);
        assert_eq!(desk.config().rotation_dwell.get(), 4000);
        assert_eq!(desk.role(&OperatorId::new("alice")), Some(Role::Admin));
        assert_eq!(desk.role(&OperatorId::new("bob")), Some(Role::Moderator));
        assert_eq!(desk.social().message_count(), 4);
        assert_eq!(desk.social().pending_len(), 0);
        assert_eq!(desk.social().blocked_author_count(), 1);
        let m2 = desk
            .social()
            .message(&MessageKey::new("prov", "acct", "m2"))
            .unwrap();
        assert_eq!(m2.author_handle.as_deref(), Some("@bob"));
        assert_eq!(m2.moderation, ModerationState::Approved);
        let m4 = desk
            .social()
            .message(&MessageKey::new("prov", "acct", "m4"))
            .unwrap();
        assert_eq!(
            m4.moderation,
            ModerationState::Rejected {
                reason: "spam".to_owned()
            }
        );
        assert_eq!(m4.flags, [ModerationFlag::Profanity("spam".to_owned())]);
        assert!(desk.is_hidden(&MessageKey::new("prov", "acct", "m1")));
        assert_eq!(
            desk.rotation(),
            [
                MessageKey::new("prov", "acct", "m2"),
                MessageKey::new("prov", "acct", "m3"),
            ]
        );
        let on_air = desk.on_air().unwrap();
        assert_eq!(on_air.key, MessageKey::new("prov", "acct", "m2"));
        assert_eq!(on_air.since, Timestamp(8000));
        assert_eq!(on_air.source, OnAirSource::Rotation);
        assert_eq!(desk.on_air_generation(), 5);
        assert_eq!(desk.evicted_audit_entries(), 3);
        let audit: Vec<&AuditEntry> = desk.audit().collect();
        assert_eq!(audit.len(), 6);
        assert_eq!(audit[0].sequence, 6);
        assert_eq!(audit[0].at, Timestamp(7000));
        assert_eq!(audit[0].actor, Actor::Operator(OperatorId::new("alice")));
        assert_eq!(
            audit[0].action,
            AuditAction::Command(DeskCommand::Feature(MessageKey::new("prov", "acct", "m2")))
        );
        assert_eq!(audit[0].result, AuditResult::Applied);
        assert_eq!(audit[1].actor, Actor::System);
        assert_eq!(
            audit[1].action,
            AuditAction::RotationAdvanced(MessageKey::new("prov", "acct", "m2"))
        );
        assert_eq!(
            audit[2].result,
            AuditResult::Denied(DeskError::PermissionDenied {
                operator: OperatorId::new("bob"),
                role: Role::Moderator,
                required: Role::Producer,
            })
        );
        assert_eq!(
            audit[3].result,
            AuditResult::Failed(DeskError::Hidden(MessageKey::new("prov", "acct", "m3")))
        );
        assert_eq!(
            audit[4].result,
            AuditResult::Failed(DeskError::AuthorNotBlocked)
        );
        assert_eq!(
            audit[5].action,
            AuditAction::Command(DeskCommand::Reject {
                key: MessageKey::new("prov", "acct", "m4"),
                reason: "spam".to_owned(),
            })
        );
        assert_eq!(desk.to_document(), document);
    }

    #[test]
    fn desk_documents_round_trip_an_empty_state() {
        let document = "freemix-social-desk 4\n\
                        config 512 16 4 4000\n\
                        operator 616c696365 admin\n\
                        sequence 1\n\
                        rotation-cursor 0\n\
                        evicted 0\n\
                        last 0\n";
        let desk = ModerationDesk::from_document(document, social()).unwrap();
        assert!(!desk.is_hidden(&MessageKey::new("prov", "acct", "m1")));
        assert!(desk.rotation().is_empty());
        assert!(desk.on_air().is_none());
        assert_eq!(desk.on_air_generation(), 0);
        assert_eq!(desk.audit().len(), 0);
        assert_eq!(desk.to_document(), document);
    }

    #[test]
    fn desk_documents_are_strict() {
        let base = "freemix-social-desk 4\n\
                    config 512 16 4 4000\n\
                    operator 616c696365 admin\n\
                    sequence 1\n\
                    rotation-cursor 0\n\
                    evicted 0\n\
                    last 9000\n";
        let rotation = "freemix-social-desk 4\n\
                        config 512 1 4 4000\n\
                        operator 616c696365 admin\n\
                        sequence 1\n\
                        rotation-cursor 0\n\
                        evicted 0\n\
                        last 9000\n\
                        rotation 70726f76 61636374 6d32\n";
        let auditable = "freemix-social-desk 4\n\
                         config 512 16 4 4000\n\
                         operator 616c696365 admin\n\
                         sequence 3\n\
                         rotation-cursor 0\n\
                         evicted 0\n\
                         last 9000\n";
        let applied =
            "audit 1 1000 operator 616c696365 command feature 70726f76 61636374 6d32 applied\n";
        for document in [
            String::new(),
            "freemix-social-desk 4\nconfig 1 1 1 1\n".to_owned(),
            "freemix-social-desk 2\nconfig 1 1 1 1\n".to_owned(),
            "freemix-social-desk 4\n".to_owned(),
            base.replacen("config 512 16 4 4000", "config 0 16 4 4000", 1),
            base.replacen("config 512 16 4 4000", "config 512 16 4 0", 1),
            "freemix-social-desk 4\nconfig 512 16 4 4000\noperator 61 viewer\nsequence 1\nrotation-cursor 0\nevicted 0\nlast 0\n".to_owned(),
            "freemix-social-desk 4\nconfig 512 16 1 4000\noperator 61 admin\noperator 62 admin\nsequence 1\nrotation-cursor 0\nevicted 0\nlast 0\n".to_owned(),
            "freemix-social-desk 4\nconfig 512 16 4 4000\noperator 62 admin\noperator 61 admin\nsequence 1\nrotation-cursor 0\nevicted 0\nlast 0\n".to_owned(),
            "freemix-social-desk 4\nconfig 512 16 4 4000\noperator 61 admin\noperator 61 admin\nsequence 1\nrotation-cursor 0\nevicted 0\nlast 0\n".to_owned(),
            "freemix-social-desk 4\nconfig 512 16 4 4000\noperator zz admin\nsequence 1\nrotation-cursor 0\nevicted 0\nlast 0\n".to_owned(),
            base.replacen("sequence 1", "sequence 0", 1),
            base.replacen("rotation-cursor 0", "rotation-cursor 1", 1),
            base.replacen("last 9000", "last nine", 1),
            base.replacen("evicted 0", "evicted", 1),
            base.replacen("sequence 1\n", "", 1),
            base.replacen("rotation-cursor 0\n", "", 1),
            base.replacen("evicted 0\n", "", 1),
            base.replacen("last 9000\n", "", 1),
            format!("{base}operator 616c696365 admin\n"),
            format!("{base}hidden 70726f76 61636374 6d32\nhidden 70726f76 61636374 6d32\n"),
            format!(
                "{base}hidden 70726f76 61636374 6d33\nhidden 70726f76 61636374 6d32\n"
            ),
            format!("{base}hidden 70726f76 61636374 zz\n"),
            format!("{base}hidden 70726f76 61636374 6d32\nrotation 70726f76 61636374 6d32\n"),
            format!("{rotation}rotation 70726f76 61636374 6d32\n"),
            format!("{rotation}rotation 70726f76 61636374 6d33\n"),
            format!("{rotation}rotation-cursor 1\n"),
            format!("{base}on-air 70726f76 61636374 6d32 8000 rotation 0\n"),
            format!("{base}on-air 70726f76 61636374 6d32 9001 manual 1\n"),
            format!("{base}on-air 70726f76 61636374 6d32 8000 manual 1\non-air 70726f76 61636374 6d32 8000 manual 1\n"),
            format!("{base}on-air 70726f76 61636374 6d32 8000 remote 1\n"),
            format!(
                "{base}hidden 70726f76 61636374 6d32\non-air 70726f76 61636374 6d32 8000 manual 1\n"
            ),
            format!("{rotation}on-air 70726f76 61636374 6d33 8000 rotation 1\n"),
            format!("{base}{applied}"),
            format!("{auditable}audit 0 1000 system rotation-advanced 70726f76 61636374 6d32 applied\n"),
            format!("{auditable}audit 3 1000 system rotation-advanced 70726f76 61636374 6d32 applied\n"),
            format!("{auditable}audit 2 1000 system rotation-advanced 70726f76 61636374 6d32 applied\naudit 2 2000 system rotation-advanced 70726f76 61636374 6d32 applied\n"),
            format!("{auditable}audit 1 2000 system rotation-advanced 70726f76 61636374 6d32 applied\naudit 2 1000 system rotation-advanced 70726f76 61636374 6d32 applied\n"),
            format!("{auditable}audit 1 9001 system rotation-advanced 70726f76 61636374 6d32 applied\n"),
            format!("{auditable}audit 1 1000 system rotation-advanced 70726f76 61636374 6d32 unknown\n"),
            format!("{auditable}audit 1 1000 system unknown 70726f76 61636374 6d32 applied\n"),
            format!("{auditable}audit 1 1000 operator 616c696365 command approve 70726f76 61636374 6d32\n"),
            format!("{auditable}audit 1 1000 operator 616c696365 command approve 70726f76 61636374 6d32 applied extra\n"),
            format!("{auditable}audit 1 1000 operator 616c696365 command unknown 70726f76 61636374 6d32 applied\n"),
            format!("{auditable}audit 1 1000 operator 616c696365 command approve 70726f76 61636374 6d32 failed unknown-error\n"),
            format!("{auditable}audit 1 1000 operator 616c696365 command approve 70726f76 61636374 6d32 failed social unknown-error\n"),
            format!("{auditable}audit 1 1000 operator 616c696365 command set-role 616c696365 superuser applied\n"),
            format!("{auditable}{applied}on-air 70726f76 61636374 6d32 8000 manual 1\n"),
            format!("{auditable}audit 1 1000 operator 616c696365 command feature 70726f76 61636374 6d32 applied\naudit 2 2000 operator 616c696365 command feature 70726f76 61636374 6d32 applied\n").replacen("config 512 16 4", "config 1 16 4", 1),
            "freemix-social-desk 4\nconfig 512 16 4 4000\noperator 61 admin\nsequence 1\nrotation-cursor 0\nevicted 0\nlast 0\nextra\n".to_owned(),
        ] {
            assert!(
                ModerationDesk::from_document(&document, social()).is_err(),
                "{document:?}"
            );
        }
    }

    #[test]
    fn desk_audit_documents_carry_every_command_and_error() {
        let social = social();
        let mut desk = ModerationDesk::new(social, DeskConfig::default(), "alice").unwrap();
        desk.execute(
            &OperatorId::new("alice"),
            DeskCommand::ClearOnAir,
            Timestamp(10),
        )
        .unwrap();
        desk.execute(
            &OperatorId::new("alice"),
            DeskCommand::SetRole {
                operator: OperatorId::new("bob"),
                role: Role::Moderator,
            },
            Timestamp(20),
        )
        .unwrap();
        let _ = desk.execute(
            &OperatorId::new("bob"),
            DeskCommand::RemoveOperator(OperatorId::new("alice")),
            Timestamp(30),
        );
        let document = desk.to_document();
        let restored = ModerationDesk::from_document(&document, SocialAggregator::empty()).unwrap();
        assert_eq!(restored.to_document(), document);
        let audit: Vec<&AuditEntry> = restored.audit().collect();
        assert_eq!(audit.len(), 3);
        assert_eq!(audit[0].result, AuditResult::Applied);
        assert_eq!(
            audit[1].action,
            AuditAction::Command(DeskCommand::SetRole {
                operator: OperatorId::new("bob"),
                role: Role::Moderator,
            })
        );
        assert_eq!(
            audit[2].result,
            AuditResult::Denied(DeskError::PermissionDenied {
                operator: OperatorId::new("bob"),
                role: Role::Moderator,
                required: Role::Admin,
            })
        );
    }
}
