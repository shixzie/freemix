//! Transport-free browser title editor (`GX-006` slice).
//!
//! The model keeps, per title instance, the last server snapshot, the
//! snapshot the operator's drafts are based on, and one draft per edited
//! exposed field. Every value-carrying command names its base revision; a
//! stale base comes back as a conflict that keeps the drafts and shows the
//! server's current values until the operator explicitly rebases or
//! discards. Nothing is ever overwritten silently.
//!
//! # Contract with a future transport
//!
//! The current wire protocol has no title commands or title snapshots, so
//! the model is defined against [`TitleCommand`] / [`TitleReply`] and the
//! narrow [`TitleControlPort`]. A server implementing the port must:
//!
//! * apply `Save`/`Take`/`Update` fields atomically only when the title's
//!   field revision equals `base_revision`, and otherwise reply
//!   [`TitleReply::Conflict`] with its current snapshot and change nothing;
//! * reject unknown fields, duplicate fields, and type mismatches without
//!   changing anything;
//! * check the air-state precondition atomically with the field update:
//!   `Save` and `Take` require the title off air, `Update` and `OffAir`
//!   require it on air, so a cue edit can never land on program because
//!   someone else took the title in the meantime;
//! * authorize each command itself; the model's permission gating is a
//!   presentation aid, not a security boundary.
//!
//! # Gating
//!
//! Editing drafts and `Save`/`Update` need [`TITLE_EDIT_PERMISSION`];
//! `Take` and `OffAir` need [`TITLE_AIR_PERMISSION`] (and `Take` with
//! changes also needs the edit permission). `Take` and `Update` air exactly
//! what was last previewed: any draft edit, rebase, or base change after
//! [`TitleEditor::preview`] invalidates the preview.

use core::fmt;
use std::collections::BTreeMap;

use fm_client::{ConnectionState, Session};
use fm_titles::{
    Color, FieldId, FieldType, FieldValue, ImageSource, MAX_GLYPHS_PER_ELEMENT, TitleId,
    TitleScene, TitleTemplate,
};

/// Session permission that allows editing title fields.
pub const TITLE_EDIT_PERMISSION: &str = "edit_project";
/// Session permission that allows taking titles to and from program.
pub const TITLE_AIR_PERMISSION: &str = "transition";
/// Longest text field draft, matching the renderer's per-element glyph cap.
pub const MAX_TITLE_TEXT_CHARS: usize = MAX_GLYPHS_PER_ELEMENT;
/// Longest image asset reference a draft may name.
pub const MAX_IMAGE_ASSET_CHARS: usize = 1_024;
/// Most title instances one editor tracks.
pub const MAX_EDITOR_TITLES: usize = 256;
/// Most exposed fields one title snapshot may carry.
pub const MAX_EXPOSED_FIELDS: usize = 128;
/// Longest title or field display name.
pub const MAX_TITLE_NAME_CHARS: usize = 256;

/// One exposed field of a title instance as the server reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExposedField {
    pub id: FieldId,
    pub name: String,
    pub value: FieldValue,
}

impl ExposedField {
    #[must_use]
    pub const fn field_type(&self) -> FieldType {
        self.value.field_type()
    }
}

/// The server's view of one title instance.
///
/// `revision` is the title's field revision ([`TitleScene::revision`]); it
/// advances on every applied field update and not on take/off-air.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleSnapshot {
    pub title: TitleId,
    pub name: String,
    pub revision: u64,
    pub on_air: bool,
    pub fields: Vec<ExposedField>,
}

impl TitleSnapshot {
    /// Builds a snapshot from a live scene, naming fields from its template
    /// in template order. Every template field is exposed.
    #[must_use]
    pub fn from_scene(
        name: impl Into<String>,
        template: &TitleTemplate,
        scene: &TitleScene,
        on_air: bool,
    ) -> Self {
        let fields = template
            .fields
            .iter()
            .filter_map(|definition| {
                scene.field_value(definition.id).map(|value| ExposedField {
                    id: definition.id,
                    name: definition.name.clone(),
                    value: value.clone(),
                })
            })
            .collect();
        Self {
            title: scene.id(),
            name: name.into(),
            revision: scene.revision(),
            on_air,
            fields,
        }
    }

    #[must_use]
    pub fn field(&self, id: FieldId) -> Option<&ExposedField> {
        self.fields.iter().find(|field| field.id == id)
    }

    fn validate(&self) -> Result<(), SnapshotError> {
        if self.name.chars().count() > MAX_TITLE_NAME_CHARS {
            return Err(SnapshotError::NameTooLong);
        }
        if self.fields.len() > MAX_EXPOSED_FIELDS {
            return Err(SnapshotError::TooManyFields);
        }
        for (index, field) in self.fields.iter().enumerate() {
            if field.name.chars().count() > MAX_TITLE_NAME_CHARS {
                return Err(SnapshotError::NameTooLong);
            }
            if self.fields[..index]
                .iter()
                .any(|other| other.id == field.id)
            {
                return Err(SnapshotError::DuplicateField(field.id));
            }
        }
        Ok(())
    }
}

/// Why a server snapshot was refused by the editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotError {
    TooManyTitles,
    TooManyFields,
    DuplicateField(FieldId),
    NameTooLong,
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyTitles => write!(formatter, "more than {MAX_EDITOR_TITLES} titles"),
            Self::TooManyFields => write!(formatter, "more than {MAX_EXPOSED_FIELDS} fields"),
            Self::DuplicateField(field) => write!(formatter, "field {field} is listed twice"),
            Self::NameTooLong => write!(formatter, "name exceeds {MAX_TITLE_NAME_CHARS} chars"),
        }
    }
}

impl std::error::Error for SnapshotError {}

/// What the current session may do with titles.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TitleAccess {
    ready: bool,
    edit: bool,
    air: bool,
}

impl TitleAccess {
    /// Derives access from the client connection and granted session.
    /// Permissions survive a disconnect so typing is not lost, but nothing
    /// can be sent until the connection is ready again.
    #[must_use]
    pub fn from_session(state: &ConnectionState, session: Option<&Session>) -> Self {
        let has = |name: &str| {
            session.is_some_and(|session| session.permissions.iter().any(|value| value == name))
        };
        Self {
            ready: matches!(state, ConnectionState::Ready) && session.is_some(),
            edit: has(TITLE_EDIT_PERMISSION),
            air: has(TITLE_AIR_PERMISSION),
        }
    }

    #[must_use]
    pub const fn is_ready(self) -> bool {
        self.ready
    }

    #[must_use]
    pub const fn can_edit(self) -> bool {
        self.edit
    }

    #[must_use]
    pub const fn can_air(self) -> bool {
        self.air
    }
}

/// A field draft that cannot be sent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldError {
    TextTooLong {
        max_chars: usize,
    },
    InvalidNumber,
    /// Colors are `#RRGGBB` or `#RRGGBBAA`.
    InvalidColor,
    EmptyImageAsset,
    ImageAssetTooLong {
        max_chars: usize,
    },
    TypeMismatch {
        expected: FieldType,
        actual: FieldType,
    },
    /// The field no longer exists on the server.
    FieldRemoved,
}

impl fmt::Display for FieldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextTooLong { max_chars } => {
                write!(formatter, "text is longer than {max_chars} characters")
            }
            Self::InvalidNumber => formatter.write_str("enter a whole number"),
            Self::InvalidColor => formatter.write_str("enter a color as #RRGGBB or #RRGGBBAA"),
            Self::EmptyImageAsset => formatter.write_str("choose an image"),
            Self::ImageAssetTooLong { max_chars } => {
                write!(
                    formatter,
                    "image reference is longer than {max_chars} characters"
                )
            }
            Self::TypeMismatch { expected, actual } => {
                write!(formatter, "field expects {expected:?}, received {actual:?}")
            }
            Self::FieldRemoved => formatter.write_str("field was removed from the title"),
        }
    }
}

/// Parses operator input for a field of `field_type`.
///
/// # Errors
///
/// Returns the validation error the field would show.
pub fn parse_field_input(field_type: FieldType, input: &str) -> Result<FieldValue, FieldError> {
    match field_type {
        FieldType::Text => {
            if input.chars().count() > MAX_TITLE_TEXT_CHARS {
                Err(FieldError::TextTooLong {
                    max_chars: MAX_TITLE_TEXT_CHARS,
                })
            } else {
                Ok(FieldValue::Text(input.to_owned()))
            }
        }
        FieldType::Number => input
            .trim()
            .parse::<i64>()
            .map(FieldValue::Number)
            .map_err(|_| FieldError::InvalidNumber),
        FieldType::Color => parse_color(input.trim())
            .map(FieldValue::Color)
            .ok_or(FieldError::InvalidColor),
        FieldType::Image => {
            let asset = input.trim();
            if asset.is_empty() {
                Err(FieldError::EmptyImageAsset)
            } else if asset.chars().count() > MAX_IMAGE_ASSET_CHARS {
                Err(FieldError::ImageAssetTooLong {
                    max_chars: MAX_IMAGE_ASSET_CHARS,
                })
            } else {
                Ok(FieldValue::Image(ImageSource::new(asset)))
            }
        }
    }
}

/// The canonical input text for a value; [`parse_field_input`] inverts it.
#[must_use]
pub fn format_field_value(value: &FieldValue) -> String {
    match value {
        FieldValue::Text(text) => text.clone(),
        FieldValue::Number(number) => number.to_string(),
        FieldValue::Image(image) => image.asset.clone(),
        FieldValue::Color(color) => format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            color.r, color.g, color.b, color.a
        ),
    }
}

fn parse_color(input: &str) -> Option<Color> {
    let hex = input.strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |index: usize| u8::from_str_radix(&hex[index..index + 2], 16).ok();
    let alpha = if hex.len() == 8 { channel(6)? } else { 255 };
    Some(Color::new(channel(0)?, channel(2)?, channel(4)?, alpha))
}

/// An operator action on one title.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum TitleOperation {
    /// Apply drafts to a title that is off air.
    Save,
    /// Apply drafts (if any) and take the title to air.
    Take,
    /// Apply drafts to a title that is on air.
    Update,
    /// Take the title off air.
    OffAir,
}

impl TitleOperation {
    pub const ALL: [Self; 4] = [Self::Save, Self::Take, Self::Update, Self::OffAir];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Save => "Save",
            Self::Take => "Take",
            Self::Update => "Update live",
            Self::OffAir => "Off air",
        }
    }

    #[must_use]
    pub const fn accessibility_label(self) -> &'static str {
        match self {
            Self::Save => "Save title changes without going to air",
            Self::Take => "Take previewed title to air",
            Self::Update => "Update on-air title with previewed changes",
            Self::OffAir => "Take title off air",
        }
    }
}

/// Why an operation is unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleBlocked {
    NotReady,
    NotPermitted,
    /// A command for this title is awaiting its reply.
    Pending,
    /// The server moved past the drafts' base; rebase or discard first.
    Stale,
    InvalidDraft,
    NotPreviewed,
    NoChanges,
    OnAir,
    OffAir,
}

impl fmt::Display for TitleBlocked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotReady => "not connected",
            Self::NotPermitted => "not permitted for this role",
            Self::Pending => "waiting for the server",
            Self::Stale => "title changed on the server; rebase or discard your edits",
            Self::InvalidDraft => "fix invalid fields first",
            Self::NotPreviewed => "preview the changes first",
            Self::NoChanges => "no changes",
            Self::OnAir => "title is on air",
            Self::OffAir => "title is off air",
        })
    }
}

/// Failure of a local editor call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleEditorError {
    UnknownTitle(TitleId),
    UnknownField(FieldId),
    Blocked(TitleBlocked),
    /// The port answered a request other than the one sent.
    UnexpectedReply(u64),
}

impl fmt::Display for TitleEditorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTitle(title) => write!(formatter, "unknown title {title}"),
            Self::UnknownField(field) => write!(formatter, "unknown field {field}"),
            Self::Blocked(reason) => reason.fmt(formatter),
            Self::UnexpectedReply(request) => {
                write!(formatter, "unexpected reply to title request {request}")
            }
        }
    }
}

impl std::error::Error for TitleEditorError {}

/// The body of a title command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleCommandBody {
    Save {
        base_revision: u64,
        fields: Vec<(FieldId, FieldValue)>,
    },
    Take {
        base_revision: u64,
        fields: Vec<(FieldId, FieldValue)>,
    },
    Update {
        base_revision: u64,
        fields: Vec<(FieldId, FieldValue)>,
    },
    OffAir,
}

impl TitleCommandBody {
    #[must_use]
    pub const fn operation(&self) -> TitleOperation {
        match self {
            Self::Save { .. } => TitleOperation::Save,
            Self::Take { .. } => TitleOperation::Take,
            Self::Update { .. } => TitleOperation::Update,
            Self::OffAir => TitleOperation::OffAir,
        }
    }
}

/// One request to the title authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleCommand {
    pub request: u64,
    pub title: TitleId,
    pub body: TitleCommandBody,
}

/// Why the authority refused a command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleRejection {
    UnknownTitle,
    UnknownField(FieldId),
    DuplicateField(FieldId),
    TypeMismatch {
        field: FieldId,
        expected: FieldType,
        actual: FieldType,
    },
    InvalidValue(FieldId),
    AlreadyOnAir,
    NotOnAir,
    NotPermitted,
    RevisionExhausted,
}

/// The authority's answer to one [`TitleCommand`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleReply {
    /// The command took effect; `snapshot` is the state right after it.
    Applied {
        request: u64,
        snapshot: TitleSnapshot,
    },
    /// `base_revision` was stale; nothing changed.
    Conflict {
        request: u64,
        current: TitleSnapshot,
    },
    /// Refused; nothing changed. `current` is included when known.
    Rejected {
        request: u64,
        reason: TitleRejection,
        current: Option<TitleSnapshot>,
    },
}

impl TitleReply {
    #[must_use]
    pub const fn request(&self) -> u64 {
        match self {
            Self::Applied { request, .. }
            | Self::Conflict { request, .. }
            | Self::Rejected { request, .. } => *request,
        }
    }
}

/// The narrow port a future title transport implements.
pub trait TitleControlPort {
    fn submit(&mut self, command: TitleCommand) -> TitleReply;
}

/// What a reply did to the editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplyOutcome {
    Applied(TitleOperation),
    Conflict,
    Rejected(TitleRejection),
}

/// A reply that does not match the pending command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnexpectedReply {
    pub request: u64,
}

impl fmt::Display for UnexpectedReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "no pending title command {}", self.request)
    }
}

impl std::error::Error for UnexpectedReply {}

/// The last command outcome shown to the operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleNotice {
    Conflict,
    Rejected(TitleRejection),
}

/// Where a previewed value comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewSource {
    Draft,
    Live,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewField {
    pub id: FieldId,
    pub name: String,
    pub value: FieldValue,
    pub source: PreviewSource,
}

/// The exact values a `Take` or `Update` would put on air.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitlePreview {
    pub title: TitleId,
    pub base_revision: u64,
    pub fields: Vec<PreviewField>,
}

/// A draft whose base value changed on the server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldConflict {
    /// The value the draft was based on.
    pub base: FieldValue,
    /// The server's current value, `None` when the field was removed.
    pub theirs: Option<FieldValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldPresentation {
    pub id: FieldId,
    pub name: String,
    pub field_type: FieldType,
    /// The server's current value, `None` when the field was removed.
    pub live_value: Option<FieldValue>,
    /// What the input control shows: the draft text, or the base value.
    pub input: String,
    pub error: Option<FieldError>,
    pub dirty: bool,
    /// The server value moved away from this field's base value.
    pub changed_upstream: bool,
    /// Set only for a dirty field whose base moved: both values are shown.
    pub conflict: Option<FieldConflict>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationPresentation {
    pub operation: TitleOperation,
    pub state: Result<(), TitleBlocked>,
}

impl OperationPresentation {
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.state.is_ok()
    }
}

#[allow(clippy::struct_excessive_bools)] // independent indicators a view renders
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleEditorPresentation {
    pub title: TitleId,
    pub name: String,
    pub on_air: bool,
    pub live_revision: u64,
    pub base_revision: u64,
    pub dirty: bool,
    pub stale: bool,
    pub pending: bool,
    pub previewed: bool,
    pub can_edit: bool,
    pub fields: Vec<FieldPresentation>,
    pub operations: Vec<OperationPresentation>,
    pub notice: Option<TitleNotice>,
}

/// One row of the title list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleListEntry {
    pub title: TitleId,
    pub name: String,
    pub on_air: bool,
    pub dirty: bool,
    pub stale: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Draft {
    /// The field's name and type when the draft was made, so a draft for a
    /// field the server later removed can still be shown and discarded.
    name: String,
    field_type: FieldType,
    input: String,
    value: Result<FieldValue, FieldError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Pending {
    request: u64,
    operation: TitleOperation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Entry {
    live: TitleSnapshot,
    base: TitleSnapshot,
    drafts: BTreeMap<FieldId, Draft>,
    generation: u64,
    previewed: Option<(u64, u64)>,
    pending: Option<Pending>,
    notice: Option<TitleNotice>,
}

impl Entry {
    fn new(snapshot: TitleSnapshot) -> Self {
        Self {
            base: snapshot.clone(),
            live: snapshot,
            drafts: BTreeMap::new(),
            generation: 0,
            previewed: None,
            pending: None,
            notice: None,
        }
    }

    fn stale(&self) -> bool {
        self.live.revision != self.base.revision
    }

    fn dirty(&self) -> bool {
        !self.drafts.is_empty()
    }

    fn invalid(&self) -> bool {
        self.drafts.values().any(|draft| draft.value.is_err())
    }

    fn previewed(&self) -> bool {
        self.previewed == Some((self.base.revision, self.generation))
    }

    fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.previewed = None;
    }

    /// Records a newer server state. With no drafts the base follows it.
    fn observe(&mut self, snapshot: TitleSnapshot) {
        if snapshot.revision < self.live.revision {
            return;
        }
        self.live = snapshot;
        if self.drafts.is_empty() && self.live.revision != self.base.revision {
            self.base = self.live.clone();
            self.touch();
        } else if !self.stale() {
            // Same field revision: names and air state may still change.
            self.base = self.live.clone();
        }
    }

    fn drafted_fields(&self) -> Vec<(FieldId, FieldValue)> {
        self.drafts
            .iter()
            .filter_map(|(id, draft)| draft.value.clone().ok().map(|value| (*id, value)))
            .collect()
    }

    fn check(&self, operation: TitleOperation, access: TitleAccess) -> Result<(), TitleBlocked> {
        if self.pending.is_some() {
            return Err(TitleBlocked::Pending);
        }
        if !access.ready {
            return Err(TitleBlocked::NotReady);
        }
        let on_air = self.live.on_air;
        match operation {
            TitleOperation::OffAir => {
                if !access.air {
                    return Err(TitleBlocked::NotPermitted);
                }
                if !on_air {
                    return Err(TitleBlocked::OffAir);
                }
                return Ok(());
            }
            TitleOperation::Save | TitleOperation::Update if !access.edit => {
                return Err(TitleBlocked::NotPermitted);
            }
            TitleOperation::Take if !access.air || (self.dirty() && !access.edit) => {
                return Err(TitleBlocked::NotPermitted);
            }
            _ => {}
        }
        match operation {
            TitleOperation::Save | TitleOperation::Take if on_air => {
                return Err(TitleBlocked::OnAir);
            }
            TitleOperation::Update if !on_air => return Err(TitleBlocked::OffAir),
            _ => {}
        }
        if self.stale() {
            return Err(TitleBlocked::Stale);
        }
        if self.invalid() {
            return Err(TitleBlocked::InvalidDraft);
        }
        if matches!(operation, TitleOperation::Save | TitleOperation::Update) && !self.dirty() {
            return Err(TitleBlocked::NoChanges);
        }
        if matches!(operation, TitleOperation::Take | TitleOperation::Update) && !self.previewed() {
            return Err(TitleBlocked::NotPreviewed);
        }
        Ok(())
    }

    /// Re-parses every draft against the base's field types and drops
    /// drafts that now equal the base value.
    fn revalidate(&mut self) {
        let base = &self.base;
        self.drafts.retain(|id, draft| {
            let Some(field) = base.field(*id) else {
                draft.value = Err(FieldError::FieldRemoved);
                return true;
            };
            draft.value = match draft.value {
                Err(FieldError::TypeMismatch { actual, .. }) if actual != field.field_type() => {
                    Err(FieldError::TypeMismatch {
                        expected: field.field_type(),
                        actual,
                    })
                }
                _ => parse_field_input(field.field_type(), &draft.input),
            };
            draft.value.as_ref() != Ok(&field.value)
        });
    }
}

/// The browser title editor for every title instance the client can see.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TitleEditor {
    access: TitleAccess,
    titles: BTreeMap<TitleId, Entry>,
    next_request: u64,
}

impl TitleEditor {
    #[must_use]
    pub fn new(access: TitleAccess) -> Self {
        Self {
            access,
            titles: BTreeMap::new(),
            next_request: 0,
        }
    }

    pub const fn set_access(&mut self, access: TitleAccess) {
        self.access = access;
    }

    #[must_use]
    pub const fn access(&self) -> TitleAccess {
        self.access
    }

    /// Records a server snapshot. Older revisions than the one already known
    /// are ignored. Drafts are kept; if the field revision moved under them
    /// the title becomes stale.
    ///
    /// # Errors
    ///
    /// Refuses malformed or over-limit snapshots without changing anything.
    pub fn observe(&mut self, snapshot: TitleSnapshot) -> Result<(), SnapshotError> {
        snapshot.validate()?;
        if let Some(entry) = self.titles.get_mut(&snapshot.title) {
            entry.observe(snapshot);
            return Ok(());
        }
        if self.titles.len() >= MAX_EDITOR_TITLES {
            return Err(SnapshotError::TooManyTitles);
        }
        self.titles.insert(snapshot.title, Entry::new(snapshot));
        Ok(())
    }

    /// Forgets a title the server removed, including its drafts.
    pub fn remove(&mut self, title: TitleId) -> bool {
        self.titles.remove(&title).is_some()
    }

    #[must_use]
    pub fn list(&self) -> Vec<TitleListEntry> {
        self.titles
            .values()
            .map(|entry| TitleListEntry {
                title: entry.live.title,
                name: entry.live.name.clone(),
                on_air: entry.live.on_air,
                dirty: entry.dirty(),
                stale: entry.stale(),
            })
            .collect()
    }

    fn entry(&self, title: TitleId) -> Result<&Entry, TitleEditorError> {
        self.titles
            .get(&title)
            .ok_or(TitleEditorError::UnknownTitle(title))
    }

    fn entry_mut(&mut self, title: TitleId) -> Result<&mut Entry, TitleEditorError> {
        self.titles
            .get_mut(&title)
            .ok_or(TitleEditorError::UnknownTitle(title))
    }

    fn editable(&mut self, title: TitleId) -> Result<&mut Entry, TitleEditorError> {
        if !self.access.edit {
            return Err(TitleEditorError::Blocked(TitleBlocked::NotPermitted));
        }
        self.entry_mut(title)
    }

    /// Sets a field's draft from operator text. Invalid text is kept with
    /// its error; text equal to the base value clears the draft.
    ///
    /// # Errors
    ///
    /// Unknown title or field, or no edit permission.
    pub fn set_input(
        &mut self,
        title: TitleId,
        field: FieldId,
        input: &str,
    ) -> Result<(), TitleEditorError> {
        let entry = self.editable(title)?;
        let base = entry
            .base
            .field(field)
            .ok_or(TitleEditorError::UnknownField(field))?;
        let value = parse_field_input(base.field_type(), input);
        if value.as_ref() == Ok(&base.value) {
            entry.drafts.remove(&field);
        } else {
            entry.drafts.insert(
                field,
                Draft {
                    name: base.name.clone(),
                    field_type: base.field_type(),
                    input: input.to_owned(),
                    value,
                },
            );
        }
        entry.touch();
        Ok(())
    }

    /// Sets a field's draft from a typed control. A value of the wrong type
    /// is kept as an invalid draft showing the type mismatch.
    ///
    /// # Errors
    ///
    /// Unknown title or field, or no edit permission.
    pub fn set_value(
        &mut self,
        title: TitleId,
        field: FieldId,
        value: &FieldValue,
    ) -> Result<(), TitleEditorError> {
        let entry = self.editable(title)?;
        let base = entry
            .base
            .field(field)
            .ok_or(TitleEditorError::UnknownField(field))?;
        let expected = base.field_type();
        if value.field_type() == expected {
            return self.set_input(title, field, &format_field_value(value));
        }
        let name = base.name.clone();
        entry.drafts.insert(
            field,
            Draft {
                name,
                field_type: expected,
                input: format_field_value(value),
                value: Err(FieldError::TypeMismatch {
                    expected,
                    actual: value.field_type(),
                }),
            },
        );
        entry.touch();
        Ok(())
    }

    /// Drops one field's draft. If no drafts remain the base catches up with
    /// the server.
    ///
    /// # Errors
    ///
    /// Unknown title.
    pub fn discard_field(
        &mut self,
        title: TitleId,
        field: FieldId,
    ) -> Result<(), TitleEditorError> {
        let entry = self.entry_mut(title)?;
        if entry.drafts.remove(&field).is_some() {
            entry.touch();
        }
        if entry.drafts.is_empty() && entry.stale() {
            entry.base = entry.live.clone();
            entry.notice = None;
            entry.touch();
        }
        Ok(())
    }

    /// Drops every draft and adopts the server's current values.
    ///
    /// # Errors
    ///
    /// Unknown title.
    pub fn discard_all(&mut self, title: TitleId) -> Result<(), TitleEditorError> {
        let entry = self.entry_mut(title)?;
        entry.drafts.clear();
        entry.base = entry.live.clone();
        entry.notice = None;
        entry.touch();
        Ok(())
    }

    /// Keeps the drafts on top of the server's current values. Drafts are
    /// re-validated against the current field types; a draft for a removed
    /// field becomes invalid. The changes must be previewed again.
    ///
    /// # Errors
    ///
    /// Unknown title, or [`TitleBlocked::Pending`] while a command is in flight.
    pub fn rebase(&mut self, title: TitleId) -> Result<(), TitleEditorError> {
        let entry = self.entry_mut(title)?;
        if entry.pending.is_some() {
            return Err(TitleEditorError::Blocked(TitleBlocked::Pending));
        }
        entry.base = entry.live.clone();
        entry.revalidate();
        entry.notice = None;
        entry.touch();
        Ok(())
    }

    /// Shows what `Take`/`Update` would air and arms them for exactly these
    /// values.
    ///
    /// # Errors
    ///
    /// Unknown title, a stale base, or an invalid draft.
    pub fn preview(&mut self, title: TitleId) -> Result<TitlePreview, TitleEditorError> {
        let entry = self.entry_mut(title)?;
        if entry.stale() {
            return Err(TitleEditorError::Blocked(TitleBlocked::Stale));
        }
        if entry.invalid() {
            return Err(TitleEditorError::Blocked(TitleBlocked::InvalidDraft));
        }
        let fields = entry
            .base
            .fields
            .iter()
            .map(|field| match entry.drafts.get(&field.id) {
                Some(Draft {
                    value: Ok(value), ..
                }) => PreviewField {
                    id: field.id,
                    name: field.name.clone(),
                    value: value.clone(),
                    source: PreviewSource::Draft,
                },
                _ => PreviewField {
                    id: field.id,
                    name: field.name.clone(),
                    value: field.value.clone(),
                    source: PreviewSource::Live,
                },
            })
            .collect();
        entry.previewed = Some((entry.base.revision, entry.generation));
        Ok(TitlePreview {
            title,
            base_revision: entry.base.revision,
            fields,
        })
    }

    /// Whether `operation` is currently allowed on `title`.
    ///
    /// # Errors
    ///
    /// Unknown title or the blocking reason.
    pub fn check(&self, title: TitleId, operation: TitleOperation) -> Result<(), TitleEditorError> {
        self.entry(title)?
            .check(operation, self.access)
            .map_err(TitleEditorError::Blocked)
    }

    /// Builds the command for `operation` and marks it pending.
    ///
    /// # Errors
    ///
    /// Unknown title or the blocking reason; nothing changes.
    pub fn request(
        &mut self,
        title: TitleId,
        operation: TitleOperation,
    ) -> Result<TitleCommand, TitleEditorError> {
        self.check(title, operation)?;
        let request = self.next_request.wrapping_add(1);
        self.next_request = request;
        let entry = self.entry_mut(title)?;
        let base_revision = entry.base.revision;
        let fields = entry.drafted_fields();
        let body = match operation {
            TitleOperation::Save => TitleCommandBody::Save {
                base_revision,
                fields,
            },
            TitleOperation::Take => TitleCommandBody::Take {
                base_revision,
                fields,
            },
            TitleOperation::Update => TitleCommandBody::Update {
                base_revision,
                fields,
            },
            TitleOperation::OffAir => TitleCommandBody::OffAir,
        };
        entry.pending = Some(Pending { request, operation });
        entry.notice = None;
        Ok(TitleCommand {
            request,
            title,
            body,
        })
    }

    /// Applies the authority's reply to the pending command.
    ///
    /// On success, drafts equal to the applied values are cleared and drafts
    /// edited after submission stay dirty on the new base. A conflict keeps
    /// every draft and marks the title stale. A rejection keeps the drafts;
    /// a type mismatch marks the offending draft invalid.
    ///
    /// # Errors
    ///
    /// Returns [`UnexpectedReply`] when no title awaits this request.
    pub fn apply_reply(&mut self, reply: TitleReply) -> Result<ReplyOutcome, UnexpectedReply> {
        let request = reply.request();
        let entry = self
            .titles
            .values_mut()
            .find(|entry| {
                entry
                    .pending
                    .is_some_and(|pending| pending.request == request)
            })
            .ok_or(UnexpectedReply { request })?;
        let pending = entry.pending.take().ok_or(UnexpectedReply { request })?;
        Ok(match reply {
            TitleReply::Applied { snapshot, .. } => {
                if snapshot.title != entry.live.title {
                    return Err(UnexpectedReply { request });
                }
                if pending.operation == TitleOperation::OffAir {
                    entry.observe(snapshot);
                } else {
                    // The authority applied our fields at our base, so the
                    // result is our base plus our edits: safe to adopt.
                    entry.live = snapshot.clone();
                    entry.base = snapshot;
                    entry.revalidate();
                    entry.touch();
                }
                ReplyOutcome::Applied(pending.operation)
            }
            TitleReply::Conflict { current, .. } => {
                if current.title == entry.live.title {
                    entry.observe(current);
                }
                entry.notice = Some(TitleNotice::Conflict);
                ReplyOutcome::Conflict
            }
            TitleReply::Rejected {
                reason, current, ..
            } => {
                if let Some(current) = current.filter(|current| current.title == entry.live.title) {
                    entry.observe(current);
                }
                if let TitleRejection::TypeMismatch {
                    field,
                    expected,
                    actual,
                } = reason
                    && let Some(draft) = entry.drafts.get_mut(&field)
                {
                    draft.value = Err(FieldError::TypeMismatch { expected, actual });
                    entry.touch();
                }
                entry.notice = Some(TitleNotice::Rejected(reason));
                ReplyOutcome::Rejected(reason)
            }
        })
    }

    /// Requests `operation` and immediately applies the port's reply.
    ///
    /// # Errors
    ///
    /// Unknown title or the blocking reason (nothing is sent), or a reply
    /// that does not answer the request (the title is unblocked).
    pub fn dispatch(
        &mut self,
        port: &mut impl TitleControlPort,
        title: TitleId,
        operation: TitleOperation,
    ) -> Result<ReplyOutcome, TitleEditorError> {
        let command = self.request(title, operation)?;
        let request = command.request;
        let reply = port.submit(command);
        self.apply_reply(reply).map_err(|error| {
            // A port that answers a different request must not leave the
            // title blocked forever.
            if let Some(entry) = self.titles.get_mut(&title)
                && entry
                    .pending
                    .is_some_and(|pending| pending.request == request)
            {
                entry.pending = None;
            }
            TitleEditorError::UnexpectedReply(error.request)
        })
    }

    /// The editor view of one title.
    ///
    /// # Errors
    ///
    /// Unknown title.
    pub fn presentation(
        &self,
        title: TitleId,
    ) -> Result<TitleEditorPresentation, TitleEditorError> {
        let entry = self.entry(title)?;
        let mut fields: Vec<FieldPresentation> = entry
            .live
            .fields
            .iter()
            .map(|live| {
                let base = entry.base.field(live.id);
                field_presentation(
                    live.id,
                    &live.name,
                    base.map_or(live.field_type(), ExposedField::field_type),
                    Some(&live.value),
                    base.map(|base| &base.value),
                    entry.drafts.get(&live.id),
                )
            })
            .collect();
        // Drafts for fields the server no longer exposes stay visible so they
        // can be compared and discarded.
        for (id, draft) in &entry.drafts {
            if entry.live.field(*id).is_none() {
                let base = entry.base.field(*id);
                fields.push(field_presentation(
                    *id,
                    base.map_or(draft.name.as_str(), |base| base.name.as_str()),
                    base.map_or(draft.field_type, ExposedField::field_type),
                    None,
                    base.map(|base| &base.value),
                    Some(draft),
                ));
            }
        }
        Ok(TitleEditorPresentation {
            title,
            name: entry.live.name.clone(),
            on_air: entry.live.on_air,
            live_revision: entry.live.revision,
            base_revision: entry.base.revision,
            dirty: entry.dirty(),
            stale: entry.stale(),
            pending: entry.pending.is_some(),
            previewed: entry.previewed(),
            can_edit: self.access.edit,
            fields,
            operations: TitleOperation::ALL
                .into_iter()
                .map(|operation| OperationPresentation {
                    operation,
                    state: entry.check(operation, self.access),
                })
                .collect(),
            notice: entry.notice,
        })
    }
}

fn field_presentation(
    id: FieldId,
    name: &str,
    field_type: FieldType,
    live: Option<&FieldValue>,
    base: Option<&FieldValue>,
    draft: Option<&Draft>,
) -> FieldPresentation {
    let changed_upstream = live != base;
    FieldPresentation {
        id,
        name: name.to_owned(),
        field_type,
        live_value: live.cloned(),
        input: draft.map_or_else(
            || base.or(live).map(format_field_value).unwrap_or_default(),
            |draft| draft.input.clone(),
        ),
        error: draft.and_then(|draft| draft.value.as_ref().err().copied()),
        dirty: draft.is_some(),
        changed_upstream,
        conflict: match (draft, base) {
            (Some(_), Some(base)) if changed_upstream => Some(FieldConflict {
                base: base.clone(),
                theirs: live.cloned(),
            }),
            _ => None,
        },
    }
}
