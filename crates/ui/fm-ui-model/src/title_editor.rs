//! Transport-free title editor shared by the browser and Studio clients.
//!
//! The model keeps, per title input, the last server snapshot, the snapshot
//! the operator's drafts are based on, and one draft per edited exposed
//! field. Every value-carrying command names its base revision. Fields are
//! revisioned one by one (Protocol 2.97): another editor's change to a field
//! without a draft moves the base along with it, while a change to a drafted
//! field makes the title stale, and a stale base comes back as a conflict
//! that keeps the drafts and shows the server's current values until the
//! operator explicitly rebases or discards. Nothing is ever overwritten
//! silently.
//!
//! Titles are keyed by their [`InputId`], fields by [`WireTitleFieldId`],
//! and values are [`TitleFieldValue`]s, so snapshots come straight from the
//! replicated [`ProjectState`] ([`TitleEditor::sync`]).
//!
//! # Authorities
//!
//! The editor speaks [`TitleCommand`] / [`TitleReply`]. Two authorities
//! exist:
//!
//! * the live protocol, through [`TitleProtocolBridge`]: a `Save` becomes
//!   `title_fields require=off_air`, an `Update` becomes `title_fields
//!   require=on_air`, a `Take` becomes `title_take` on the title's take
//!   channel, and `OffAir` becomes `title_off_air`. The bridge maps command
//!   results back and holds an acceptance until the replicated state has
//!   applied its revision;
//! * any synchronous [`TitleControlPort`] (used by tests), which must:
//!   apply `Save`/`Take`/`Update` fields atomically only when no field they
//!   set changed after `base_revision` (and the title has reached it), and
//!   otherwise reply
//!   [`TitleReply::Conflict`] with its current snapshot and change nothing;
//!   reject unknown fields, duplicate fields, and type mismatches without
//!   changing anything; check the air-state precondition atomically with the
//!   field update (`Save` and `Take` require the title off air, `Update` and
//!   `OffAir` require it on air); and authorize each command itself.
//!
//! # Gating
//!
//! Editing drafts and `Save`/`Update` need [`TITLE_EDIT_PERMISSION`];
//! `Take` and `OffAir` need [`TITLE_AIR_PERMISSION`] (and `Take` with
//! changes also needs the edit permission). `Take` and `Update` air exactly
//! what was last previewed: any draft edit, rebase, or base change after
//! [`TitleEditor::preview`] invalidates the preview. The gating is a
//! presentation aid; the server authorizes every command.

use core::fmt;
use std::collections::BTreeMap;

use fm_protocol::{TitleFieldValue, WireOverlayChannelId, WireTitleFieldId};
use fm_types::InputId;

use crate::{ProjectState, TitleStatus};

mod bridge;

pub use bridge::{
    BridgeIntake, InvalidTakeChannel, TitleProtocolBridge, WireTitleOutcome, classify_rejection,
    title_payload,
};

/// Session permission that allows editing title fields.
pub const TITLE_EDIT_PERMISSION: &str = "edit_project";
/// Session permission that allows taking titles to and from program.
pub const TITLE_AIR_PERMISSION: &str = "transition";
/// Longest text field draft, matching the protocol's text value bound.
pub const MAX_TITLE_TEXT_CHARS: usize = fm_protocol::MAX_TITLE_TEXT_CHARS;
/// Largest total of a title's text values plus image asset references, in
/// UTF-8 bytes, matching the protocol and title domain bound.
pub const MAX_TITLE_CONTENT_BYTES: usize = fm_protocol::MAX_TITLE_CONTENT_BYTES;
/// Longest image asset reference a draft may name.
pub const MAX_IMAGE_ASSET_CHARS: usize = 1_024;
/// Most title instances one editor tracks.
pub const MAX_EDITOR_TITLES: usize = 256;
/// Most exposed fields one title snapshot may carry.
pub const MAX_EXPOSED_FIELDS: usize = fm_protocol::MAX_TITLE_FIELDS;
/// Longest title or field display name.
pub const MAX_TITLE_NAME_CHARS: usize = 256;
/// Overlay channel a title is taken onto until the operator picks another.
pub const DEFAULT_TAKE_CHANNEL: u8 = 1;

/// The type of a title field value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FieldType {
    Text,
    Number,
    Color,
    Image,
}

impl FieldType {
    /// The type of `value`.
    #[must_use]
    pub const fn of(value: &TitleFieldValue) -> Self {
        match value {
            TitleFieldValue::Text(_) => Self::Text,
            TitleFieldValue::Number(_) => Self::Number,
            TitleFieldValue::Color { .. } => Self::Color,
            TitleFieldValue::Image { .. } => Self::Image,
        }
    }
}

/// One exposed field of a title as the server reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExposedField {
    pub id: WireTitleFieldId,
    pub name: String,
    pub value: TitleFieldValue,
    /// The title field revision that last changed the value (Protocol
    /// 2.97): a draft of this field is stale once it passes the draft's base.
    pub revision: u64,
}

impl ExposedField {
    #[must_use]
    pub const fn field_type(&self) -> FieldType {
        FieldType::of(&self.value)
    }
}

/// The server's view of one title input.
///
/// `revision` is the title's field revision; it advances on every applied
/// field update and not on take/off-air.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleSnapshot {
    pub title: InputId,
    pub name: String,
    pub revision: u64,
    pub on_air: bool,
    pub fields: Vec<ExposedField>,
}

impl TitleSnapshot {
    /// Builds a snapshot from one replicated title. `None` while the
    /// snapshot that announced it still awaits its `title_state`.
    #[must_use]
    pub fn from_status(status: &TitleStatus, on_air: bool) -> Option<Self> {
        status.complete.then(|| Self {
            title: status.input,
            name: status.name.clone(),
            revision: status.field_revision,
            on_air,
            fields: status
                .fields
                .iter()
                .map(|field| ExposedField {
                    id: field.field,
                    name: field.name.clone(),
                    value: field.value.clone(),
                    revision: field.revision,
                })
                .collect(),
        })
    }

    /// Builds the snapshot of `title` from replicated project state, with
    /// its air state from [`ProjectState::title_on_air`].
    #[must_use]
    pub fn from_state(state: &ProjectState, title: InputId) -> Option<Self> {
        Self::from_status(state.title(title)?, state.title_on_air(title))
    }

    #[must_use]
    pub fn field(&self, id: WireTitleFieldId) -> Option<&ExposedField> {
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
    DuplicateField(WireTitleFieldId),
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
    #[must_use]
    pub const fn new(ready: bool, edit: bool, air: bool) -> Self {
        Self { ready, edit, air }
    }

    /// Derives access from the granted session permission names. `ready` is
    /// whether commands can be sent now; permissions survive a disconnect
    /// so typing is not lost.
    #[must_use]
    pub fn from_permissions<S: AsRef<str>>(ready: bool, permissions: &[S]) -> Self {
        let has = |name: &str| permissions.iter().any(|value| value.as_ref() == name);
        Self {
            ready,
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
pub fn parse_field_input(
    field_type: FieldType,
    input: &str,
) -> Result<TitleFieldValue, FieldError> {
    match field_type {
        FieldType::Text => {
            if input.chars().count() > MAX_TITLE_TEXT_CHARS {
                Err(FieldError::TextTooLong {
                    max_chars: MAX_TITLE_TEXT_CHARS,
                })
            } else {
                Ok(TitleFieldValue::Text(input.to_owned()))
            }
        }
        FieldType::Number => input
            .trim()
            .parse::<i64>()
            .map(TitleFieldValue::Number)
            .map_err(|_| FieldError::InvalidNumber),
        FieldType::Color => parse_color(input.trim()).ok_or(FieldError::InvalidColor),
        FieldType::Image => {
            let asset = input.trim();
            if asset.is_empty() {
                Err(FieldError::EmptyImageAsset)
            } else if asset.chars().count() > MAX_IMAGE_ASSET_CHARS {
                Err(FieldError::ImageAssetTooLong {
                    max_chars: MAX_IMAGE_ASSET_CHARS,
                })
            } else {
                Ok(TitleFieldValue::Image {
                    asset_uri: asset.to_owned(),
                })
            }
        }
    }
}

/// The canonical input text for a value; [`parse_field_input`] inverts it.
#[must_use]
pub fn format_field_value(value: &TitleFieldValue) -> String {
    match value {
        TitleFieldValue::Text(text) => text.clone(),
        TitleFieldValue::Number(number) => number.to_string(),
        TitleFieldValue::Image { asset_uri } => asset_uri.clone(),
        TitleFieldValue::Color {
            red,
            green,
            blue,
            alpha,
        } => format!("#{red:02X}{green:02X}{blue:02X}{alpha:02X}"),
    }
}

fn parse_color(input: &str) -> Option<TitleFieldValue> {
    let hex = input.strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |index: usize| u8::from_str_radix(&hex[index..index + 2], 16).ok();
    let alpha = if hex.len() == 8 { channel(6)? } else { 255 };
    Some(TitleFieldValue::Color {
        red: channel(0)?,
        green: channel(2)?,
        blue: channel(4)?,
        alpha,
    })
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
    /// The title's text and image content with the drafts applied would
    /// exceed [`MAX_TITLE_CONTENT_BYTES`].
    ContentTooLarge,
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
            Self::ContentTooLarge => "the title content is too large",
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
    UnknownTitle(InputId),
    UnknownField(WireTitleFieldId),
    Blocked(TitleBlocked),
    /// The port answered a request other than the one sent.
    UnexpectedReply(u64),
    /// Not an overlay channel a title can be taken onto.
    InvalidTakeChannel(u8),
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
            Self::InvalidTakeChannel(channel) => {
                write!(formatter, "overlay channel {channel} does not exist")
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
        fields: Vec<(WireTitleFieldId, TitleFieldValue)>,
    },
    Take {
        /// Overlay channel number the title is taken onto.
        channel: u8,
        base_revision: u64,
        fields: Vec<(WireTitleFieldId, TitleFieldValue)>,
    },
    Update {
        base_revision: u64,
        fields: Vec<(WireTitleFieldId, TitleFieldValue)>,
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
    pub title: InputId,
    pub body: TitleCommandBody,
}

/// Why the authority refused a command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleRejection {
    UnknownTitle,
    UnknownField(WireTitleFieldId),
    DuplicateField(WireTitleFieldId),
    TypeMismatch {
        field: WireTitleFieldId,
        expected: FieldType,
        actual: FieldType,
    },
    InvalidValue(WireTitleFieldId),
    /// The field list was refused (unknown, duplicate, mistyped, or invalid
    /// field) by an authority that does not name the field, as the wire
    /// protocol does not.
    InvalidFields,
    /// The title's text and image content would exceed its byte bound.
    ContentTooLarge,
    AlreadyOnAir,
    NotOnAir,
    /// Off air was refused because the title is on the Program bus.
    OnProgram,
    /// Off air was refused because several overlay channels show the title.
    OnMultipleOverlays,
    /// A project or input edit padlock refused the field change.
    Locked,
    NotPermitted,
    RevisionExhausted,
    /// Any other refusal; the command result carries the server's message.
    Other,
}

impl fmt::Display for TitleRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTitle => formatter.write_str("the title no longer exists"),
            Self::UnknownField(field) => write!(formatter, "the title has no field {field}"),
            Self::DuplicateField(field) => write!(formatter, "field {field} is set twice"),
            Self::TypeMismatch {
                field,
                expected,
                actual,
            } => write!(
                formatter,
                "field {field} expects {expected:?}, received {actual:?}"
            ),
            Self::InvalidValue(field) => write!(formatter, "field {field} has an invalid value"),
            Self::InvalidFields => formatter.write_str("the server refused the field values"),
            Self::ContentTooLarge => formatter.write_str("the title content is too large"),
            Self::AlreadyOnAir => formatter.write_str("the title is already on air"),
            Self::NotOnAir => formatter.write_str("the title is not on air"),
            Self::OnProgram => {
                formatter.write_str("the title is on Program; switch Program away instead")
            }
            Self::OnMultipleOverlays => formatter
                .write_str("the title is on several overlay channels; take each off directly"),
            Self::Locked => formatter.write_str("the project or title is locked for editing"),
            Self::NotPermitted => formatter.write_str("not permitted for this role"),
            Self::RevisionExhausted => formatter.write_str("the title revision is exhausted"),
            Self::Other => formatter.write_str("the server refused the command"),
        }
    }
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

/// A synchronous title authority, answering each command immediately.
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
    pub id: WireTitleFieldId,
    pub name: String,
    pub value: TitleFieldValue,
    pub source: PreviewSource,
}

/// The exact values a `Take` or `Update` would put on air.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitlePreview {
    pub title: InputId,
    pub base_revision: u64,
    pub fields: Vec<PreviewField>,
}

/// A draft whose base value changed on the server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldConflict {
    /// The value the draft was based on.
    pub base: TitleFieldValue,
    /// The server's current value, `None` when the field was removed.
    pub theirs: Option<TitleFieldValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldPresentation {
    pub id: WireTitleFieldId,
    pub name: String,
    pub field_type: FieldType,
    /// The server's current value, `None` when the field was removed.
    pub live_value: Option<TitleFieldValue>,
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
    pub title: InputId,
    pub name: String,
    pub on_air: bool,
    pub live_revision: u64,
    pub base_revision: u64,
    pub dirty: bool,
    pub stale: bool,
    pub pending: bool,
    pub previewed: bool,
    pub can_edit: bool,
    /// Overlay channel number a `Take` uses.
    pub take_channel: u8,
    pub fields: Vec<FieldPresentation>,
    pub operations: Vec<OperationPresentation>,
    pub notice: Option<TitleNotice>,
}

/// One row of the title list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleListEntry {
    pub title: InputId,
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
    value: Result<TitleFieldValue, FieldError>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Pending {
    request: u64,
    operation: TitleOperation,
    /// The field values the command carried.
    fields: Vec<(WireTitleFieldId, TitleFieldValue)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Entry {
    live: TitleSnapshot,
    base: TitleSnapshot,
    drafts: BTreeMap<WireTitleFieldId, Draft>,
    generation: u64,
    previewed: Option<(u64, u64)>,
    pending: Option<Pending>,
    notice: Option<TitleNotice>,
    take_channel: u8,
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
            take_channel: DEFAULT_TAKE_CHANNEL,
        }
    }

    /// Whether the server moved past the drafts' base at all.
    fn behind(&self) -> bool {
        self.live.revision != self.base.revision
    }

    /// Whether the server changed `field` after the drafts' base: its field
    /// revision passed the base (Protocol 2.97), its value or type moved,
    /// or it is no longer there.
    fn changed_upstream(&self, field: WireTitleFieldId) -> bool {
        match (self.live.field(field), self.base.field(field)) {
            (Some(live), Some(base)) => {
                live.revision > self.base.revision
                    || live.value != base.value
                    || live.field_type() != base.field_type()
            }
            // Gone from both: the draft is invalid, not stale.
            (None, None) => false,
            _ => true,
        }
    }

    /// Whether a draft is based on a value someone else has since changed.
    /// Changes to fields without a draft never make a title stale: the base
    /// follows them (see [`Self::adopt_live`]), as the authority accepts an
    /// edit that touches only fields unchanged since its base revision. A
    /// server that went back to an older revision leaves drafts stale.
    fn stale(&self) -> bool {
        (self.dirty() && self.live.revision < self.base.revision)
            || self
                .drafts
                .keys()
                .any(|field| self.changed_upstream(*field))
    }

    /// Moves the base up to the server's state when no draft conflicts with
    /// it; other fields' changes then need a fresh preview.
    fn catch_up(&mut self) {
        if self.behind() && !self.stale() {
            self.base = self.live.clone();
            self.revalidate();
            self.touch();
        }
    }

    fn dirty(&self) -> bool {
        !self.drafts.is_empty()
    }

    fn invalid(&self) -> bool {
        self.drafts.values().any(|draft| draft.value.is_err())
    }

    /// Whether the title's text and image content, with the valid drafts
    /// applied, exceeds the bound the authority enforces on the whole title.
    fn content_too_large(&self) -> bool {
        self.base
            .fields
            .iter()
            .map(|field| match self.drafts.get(&field.id) {
                Some(Draft {
                    value: Ok(value), ..
                }) => value.content_bytes(),
                _ => field.value.content_bytes(),
            })
            .fold(0_usize, usize::saturating_add)
            > MAX_TITLE_CONTENT_BYTES
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
        self.adopt_live(snapshot);
    }

    /// Records the server state whatever its revision. The base follows it
    /// unless a draft's field changed under it.
    fn adopt_live(&mut self, snapshot: TitleSnapshot) {
        self.live = snapshot;
        if self.behind() {
            self.catch_up();
        } else if !self.stale() {
            // Same field revision: names and air state may still change.
            self.base = self.live.clone();
        }
    }

    /// Adopts the state after the authority applied `fields` at our base.
    /// Drafts the command carried are applied, unless edited since. When
    /// the state also holds another editor's later change and drafts
    /// remain, the base becomes our own result so the change reads as a
    /// conflict instead of silently becoming the drafts' base.
    fn applied(
        &mut self,
        fields: Vec<(WireTitleFieldId, TitleFieldValue)>,
        snapshot: TitleSnapshot,
    ) {
        self.drafts.retain(|id, draft| {
            !fields
                .iter()
                .any(|(field, value)| field == id && draft.value.as_ref() == Ok(value))
        });
        let mut ours = self.base.clone();
        for (id, value) in fields {
            if let Some(field) = ours.fields.iter_mut().find(|field| field.id == id) {
                field.value = value;
            }
        }
        // Only another editor's change to a field still drafted conflicts
        // (Protocol 2.97); changes to other fields become the base.
        let concurrent = self.drafts.keys().any(|id| {
            ours.field(*id).map(|field| &field.value)
                != snapshot.field(*id).map(|field| &field.value)
        });
        if concurrent && ours.revision < snapshot.revision {
            ours.name.clone_from(&snapshot.name);
            ours.on_air = snapshot.on_air;
            self.base = ours;
        } else {
            self.base = snapshot.clone();
        }
        self.live = snapshot;
        self.revalidate();
        self.touch();
    }

    fn drafted_fields(&self) -> Vec<(WireTitleFieldId, TitleFieldValue)> {
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
        if self.content_too_large() {
            return Err(TitleBlocked::ContentTooLarge);
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

/// The title editor for every title input the client can see.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TitleEditor {
    access: TitleAccess,
    titles: BTreeMap<InputId, Entry>,
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

    /// Observes every complete title in replicated `state` and forgets
    /// titles the state no longer has. Titles still awaiting their
    /// `title_state` keep their last known snapshot.
    ///
    /// The replicated state is authoritative: a field revision lower than
    /// the one already known (an authority restarted from an older save) is
    /// adopted rather than ignored, and leaves any drafts stale.
    ///
    /// # Errors
    ///
    /// Returns the first refused snapshot; every other title is still
    /// observed.
    pub fn sync(&mut self, state: &ProjectState) -> Result<(), SnapshotError> {
        self.titles.retain(|title, _| state.title(*title).is_some());
        let mut first_error = None;
        for status in state.titles() {
            if let Some(snapshot) =
                TitleSnapshot::from_status(status, state.title_on_air(status.input))
                && let Err(error) = self.observe_authoritative(snapshot)
            {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn observe_authoritative(&mut self, snapshot: TitleSnapshot) -> Result<(), SnapshotError> {
        snapshot.validate()?;
        if let Some(entry) = self.titles.get_mut(&snapshot.title) {
            if snapshot.revision < entry.live.revision {
                entry.adopt_live(snapshot);
                return Ok(());
            }
            entry.observe(snapshot);
            return Ok(());
        }
        self.observe(snapshot)
    }

    /// Forgets a title the server removed, including its drafts.
    pub fn remove(&mut self, title: InputId) -> bool {
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

    fn entry(&self, title: InputId) -> Result<&Entry, TitleEditorError> {
        self.titles
            .get(&title)
            .ok_or(TitleEditorError::UnknownTitle(title))
    }

    fn entry_mut(&mut self, title: InputId) -> Result<&mut Entry, TitleEditorError> {
        self.titles
            .get_mut(&title)
            .ok_or(TitleEditorError::UnknownTitle(title))
    }

    fn editable(&mut self, title: InputId) -> Result<&mut Entry, TitleEditorError> {
        if !self.access.edit {
            return Err(TitleEditorError::Blocked(TitleBlocked::NotPermitted));
        }
        self.entry_mut(title)
    }

    /// The overlay channel number a `Take` of `title` uses.
    ///
    /// # Errors
    ///
    /// Unknown title.
    pub fn take_channel(&self, title: InputId) -> Result<u8, TitleEditorError> {
        Ok(self.entry(title)?.take_channel)
    }

    /// Chooses the overlay channel a `Take` of `title` uses. This does not
    /// change what is previewed.
    ///
    /// # Errors
    ///
    /// Unknown title, or a channel number the protocol has no channel for.
    pub fn set_take_channel(
        &mut self,
        title: InputId,
        channel: u8,
    ) -> Result<(), TitleEditorError> {
        if WireOverlayChannelId::new(channel).is_none() {
            return Err(TitleEditorError::InvalidTakeChannel(channel));
        }
        self.entry_mut(title)?.take_channel = channel;
        Ok(())
    }

    /// Sets a field's draft from operator text. Invalid text is kept with
    /// its error; text equal to the base value clears the draft.
    ///
    /// # Errors
    ///
    /// Unknown title or field, or no edit permission.
    pub fn set_input(
        &mut self,
        title: InputId,
        field: WireTitleFieldId,
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
        title: InputId,
        field: WireTitleFieldId,
        value: &TitleFieldValue,
    ) -> Result<(), TitleEditorError> {
        let entry = self.editable(title)?;
        let base = entry
            .base
            .field(field)
            .ok_or(TitleEditorError::UnknownField(field))?;
        let expected = base.field_type();
        let actual = FieldType::of(value);
        if actual == expected {
            return self.set_input(title, field, &format_field_value(value));
        }
        let name = base.name.clone();
        entry.drafts.insert(
            field,
            Draft {
                name,
                field_type: expected,
                input: format_field_value(value),
                value: Err(FieldError::TypeMismatch { expected, actual }),
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
        title: InputId,
        field: WireTitleFieldId,
    ) -> Result<(), TitleEditorError> {
        let entry = self.entry_mut(title)?;
        if entry.drafts.remove(&field).is_some() {
            entry.touch();
        }
        // Without the discarded draft the rest may no longer conflict.
        if entry.behind() && !entry.stale() {
            if entry.drafts.is_empty() {
                entry.notice = None;
            }
            entry.catch_up();
        }
        Ok(())
    }

    /// Drops every draft and adopts the server's current values.
    ///
    /// # Errors
    ///
    /// Unknown title.
    pub fn discard_all(&mut self, title: InputId) -> Result<(), TitleEditorError> {
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
    pub fn rebase(&mut self, title: InputId) -> Result<(), TitleEditorError> {
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
    pub fn preview(&mut self, title: InputId) -> Result<TitlePreview, TitleEditorError> {
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
    pub fn check(&self, title: InputId, operation: TitleOperation) -> Result<(), TitleEditorError> {
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
        title: InputId,
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
                channel: entry.take_channel,
                base_revision,
                fields,
            },
            TitleOperation::Update => TitleCommandBody::Update {
                base_revision,
                fields,
            },
            TitleOperation::OffAir => TitleCommandBody::OffAir,
        };
        let submitted = match &body {
            TitleCommandBody::Save { fields, .. }
            | TitleCommandBody::Take { fields, .. }
            | TitleCommandBody::Update { fields, .. } => fields.clone(),
            TitleCommandBody::OffAir => Vec::new(),
        };
        entry.pending = Some(Pending {
            request,
            operation,
            fields: submitted,
        });
        entry.notice = None;
        Ok(TitleCommand {
            request,
            title,
            body,
        })
    }

    /// Unblocks a title whose command will never be answered (its transport
    /// was lost and the result is unknown). Drafts are kept; the next
    /// replicated snapshot shows whether it took effect.
    pub fn abandon(&mut self, request: u64) -> bool {
        self.titles.values_mut().any(|entry| {
            let matches = entry
                .pending
                .as_ref()
                .is_some_and(|pending| pending.request == request);
            if matches {
                entry.pending = None;
            }
            matches
        })
    }

    /// Unblocks every title with a command in flight, as [`Self::abandon`]
    /// does, when the transport that would answer them is gone. Returns how
    /// many were pending.
    pub fn abandon_all(&mut self) -> usize {
        self.titles
            .values_mut()
            .filter_map(|entry| entry.pending.take())
            .count()
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
                    .as_ref()
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
                    entry.applied(pending.fields, snapshot);
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
        title: InputId,
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
                    .as_ref()
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
        title: InputId,
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
            take_channel: entry.take_channel,
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
    id: WireTitleFieldId,
    name: &str,
    field_type: FieldType,
    live: Option<&TitleFieldValue>,
    base: Option<&TitleFieldValue>,
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

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;

    const TITLE: u128 = 3;

    fn title() -> InputId {
        InputId::new(NonZeroU128::new(TITLE).unwrap())
    }

    fn field(value: u128) -> WireTitleFieldId {
        WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
    }

    fn text(value: &str) -> TitleFieldValue {
        TitleFieldValue::Text(value.into())
    }

    fn snapshot(revision: u64, values: [&str; 2]) -> TitleSnapshot {
        TitleSnapshot {
            title: title(),
            name: "Lower third".into(),
            revision,
            on_air: false,
            fields: values
                .iter()
                .zip(1..)
                .map(|(value, id)| ExposedField {
                    id: field(id),
                    name: format!("Field {id}"),
                    value: text(value),
                    revision: 0,
                })
                .collect(),
        }
    }

    fn new_editor(revision: u64, values: [&str; 2]) -> TitleEditor {
        let mut editor = TitleEditor::new(TitleAccess::new(true, true, true));
        editor.observe(snapshot(revision, values)).unwrap();
        editor
    }

    fn field_view(editor: &TitleEditor, id: u128) -> FieldPresentation {
        editor
            .presentation(title())
            .unwrap()
            .fields
            .into_iter()
            .find(|candidate| candidate.id == field(id))
            .unwrap()
    }

    #[test]
    fn an_applied_reply_never_silently_rebases_drafts_onto_a_later_edit() {
        // Our save applies field 1 at revision 2; before the reply is
        // released another editor changes field 2 (revision 3), while this
        // operator keeps typing in field 2.
        let mut editor = new_editor(1, ["Ana", "x"]);
        editor.set_input(title(), field(1), "Bea").unwrap();
        let command = editor.request(title(), TitleOperation::Save).unwrap();
        editor.set_input(title(), field(2), "y").unwrap();
        let outcome = editor.apply_reply(TitleReply::Applied {
            request: command.request,
            snapshot: snapshot(3, ["Bea", "z"]),
        });
        assert_eq!(outcome, Ok(ReplyOutcome::Applied(TitleOperation::Save)));
        let view = editor.presentation(title()).unwrap();
        assert!(
            view.stale,
            "the other editor's change must read as a conflict"
        );
        assert!(!field_view(&editor, 1).dirty);
        let typed = field_view(&editor, 2);
        assert!(typed.dirty);
        assert_eq!(
            typed.conflict,
            Some(FieldConflict {
                base: text("x"),
                theirs: Some(text("z")),
            })
        );
        assert_eq!(
            editor.check(title(), TitleOperation::Save),
            Err(TitleEditorError::Blocked(TitleBlocked::Stale))
        );

        // A submitted draft that was applied and then overwritten by
        // another editor is not left behind as an unsent edit.
        let mut editor = new_editor(1, ["Ana", "x"]);
        editor.set_input(title(), field(1), "Bea").unwrap();
        let command = editor.request(title(), TitleOperation::Save).unwrap();
        editor
            .apply_reply(TitleReply::Applied {
                request: command.request,
                snapshot: snapshot(3, ["Cy", "x"]),
            })
            .unwrap();
        let view = editor.presentation(title()).unwrap();
        assert!(!view.dirty && !view.stale);
        assert_eq!(view.base_revision, 3);
        assert_eq!(field_view(&editor, 1).input, "Cy");
    }

    #[test]
    fn drafts_over_the_title_content_bound_cannot_be_sent() {
        let mut editor = new_editor(1, ["Ana", "x"]);
        // 3,000 four-byte scalars are within the per-text character bound
        // but 12,000 bytes; with the other field the title exceeds 12,288.
        let wide = "\u{1F600}".repeat(3_000);
        editor.set_input(title(), field(1), &wide).unwrap();
        assert_eq!(field_view(&editor, 1).error, None);
        assert_eq!(editor.check(title(), TitleOperation::Save), Ok(()));
        editor
            .set_input(title(), field(2), &"a".repeat(289))
            .unwrap();
        for operation in [TitleOperation::Save, TitleOperation::Take] {
            assert_eq!(
                editor.check(title(), operation),
                Err(TitleEditorError::Blocked(TitleBlocked::ContentTooLarge)),
                "{operation:?}"
            );
        }
        assert_eq!(
            editor.request(title(), TitleOperation::Save),
            Err(TitleEditorError::Blocked(TitleBlocked::ContentTooLarge))
        );
        editor
            .set_input(title(), field(2), &"a".repeat(288))
            .unwrap();
        assert_eq!(editor.check(title(), TitleOperation::Save), Ok(()));
    }

    #[test]
    fn an_authoritative_lower_revision_is_adopted() {
        let mut editor = new_editor(5, ["Ana", "x"]);
        // A restarted authority reports revision 2: clean titles follow it.
        editor
            .observe_authoritative(snapshot(2, ["Old", "x"]))
            .unwrap();
        let view = editor.presentation(title()).unwrap();
        assert_eq!((view.live_revision, view.base_revision), (2, 2));
        assert_eq!(field_view(&editor, 1).input, "Old");
        // A plain out-of-order observation is still ignored.
        editor.observe(snapshot(1, ["Older", "x"])).unwrap();
        assert_eq!(editor.presentation(title()).unwrap().live_revision, 2);

        // Drafts on the lost revision become stale and can be rebased.
        let mut editor = new_editor(5, ["Ana", "x"]);
        editor.set_input(title(), field(1), "Bea").unwrap();
        editor
            .observe_authoritative(snapshot(2, ["Old", "x"]))
            .unwrap();
        assert!(editor.presentation(title()).unwrap().stale);
        editor.rebase(title()).unwrap();
        let command = editor.request(title(), TitleOperation::Save).unwrap();
        assert_eq!(
            command.body,
            TitleCommandBody::Save {
                base_revision: 2,
                fields: vec![(field(1), text("Bea"))],
            }
        );
    }

    #[test]
    fn abandon_all_unblocks_every_pending_title() {
        let mut editor = new_editor(1, ["Ana", "x"]);
        editor.set_input(title(), field(1), "Bea").unwrap();
        let command = editor.request(title(), TitleOperation::Save).unwrap();
        assert_eq!(editor.abandon_all(), 1);
        assert_eq!(editor.abandon_all(), 0);
        assert!(editor.presentation(title()).unwrap().dirty);
        assert_eq!(editor.check(title(), TitleOperation::Save), Ok(()));
        assert_eq!(
            editor.apply_reply(TitleReply::Conflict {
                request: command.request,
                current: snapshot(1, ["Ana", "x"]),
            }),
            Err(UnexpectedReply {
                request: command.request
            })
        );
    }
}
