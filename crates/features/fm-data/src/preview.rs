//! Preview/test state for title bindings: a staged mapping result that can
//! be inspected and diffed against the live (on-air) values, then committed
//! atomically with a revision counter.

use std::{collections::BTreeMap, fmt};

use crate::{BindingReport, BoundValue};

/// One field difference between live and staged values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldChange {
    Added {
        field: String,
        value: BoundValue,
    },
    Removed {
        field: String,
        previous: BoundValue,
    },
    Changed {
        field: String,
        previous: BoundValue,
        value: BoundValue,
    },
}

impl FieldChange {
    #[must_use]
    pub fn field(&self) -> &str {
        match self {
            Self::Added { field, .. }
            | Self::Removed { field, .. }
            | Self::Changed { field, .. } => field,
        }
    }
}

/// Ordered (by field name) changes from `live` to `next`.
#[must_use]
pub fn diff_values(
    live: &BTreeMap<String, BoundValue>,
    next: &BTreeMap<String, BoundValue>,
) -> Vec<FieldChange> {
    let mut changes = Vec::new();
    let mut live_iter = live.iter().peekable();
    let mut next_iter = next.iter().peekable();
    loop {
        match (live_iter.peek(), next_iter.peek()) {
            (None, None) => break,
            (Some((field, previous)), None) => {
                changes.push(FieldChange::Removed {
                    field: (*field).clone(),
                    previous: (*previous).clone(),
                });
                live_iter.next();
            }
            (None, Some((field, value))) => {
                changes.push(FieldChange::Added {
                    field: (*field).clone(),
                    value: (*value).clone(),
                });
                next_iter.next();
            }
            (Some((live_field, previous)), Some((next_field, value))) => {
                match live_field.cmp(next_field) {
                    std::cmp::Ordering::Less => {
                        changes.push(FieldChange::Removed {
                            field: (*live_field).clone(),
                            previous: (*previous).clone(),
                        });
                        live_iter.next();
                    }
                    std::cmp::Ordering::Greater => {
                        changes.push(FieldChange::Added {
                            field: (*next_field).clone(),
                            value: (*value).clone(),
                        });
                        next_iter.next();
                    }
                    std::cmp::Ordering::Equal => {
                        if previous != value {
                            changes.push(FieldChange::Changed {
                                field: (*live_field).clone(),
                                previous: (*previous).clone(),
                                value: (*value).clone(),
                            });
                        }
                        live_iter.next();
                        next_iter.next();
                    }
                }
            }
        }
    }
    changes
}

/// How a staged report with errored fields becomes the next live values.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StagePolicy {
    /// Staging an incomplete report is rejected; live stays untouched.
    #[default]
    RequireComplete,
    /// Errored fields keep their current live value (or stay absent).
    KeepLiveOnError,
}

/// A staged mapping result awaiting commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedPreview {
    /// Live revision the stage was computed against.
    pub base_revision: u64,
    pub report: BindingReport,
    /// The exact values that become live on commit.
    pub next: BTreeMap<String, BoundValue>,
    /// Changes from live (at `base_revision`) to `next`.
    pub changes: Vec<FieldChange>,
}

/// Preview staging or commit failed; live values are unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreviewError {
    Incomplete {
        errors: Vec<String>,
    },
    NothingStaged,
    /// Live changed after staging; re-stage before committing.
    Stale {
        staged: u64,
        live: u64,
    },
}

impl fmt::Display for PreviewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete { errors } => {
                write!(
                    formatter,
                    "staged mapping has errors in {}",
                    errors.join(", ")
                )
            }
            Self::NothingStaged => formatter.write_str("nothing is staged"),
            Self::Stale { staged, live } => write!(
                formatter,
                "staged against revision {staged} but live is revision {live}"
            ),
        }
    }
}

impl std::error::Error for PreviewError {}

/// Most fields one preview document may hold.
pub const MAX_PREVIEW_FIELDS: usize = 256;
/// Largest field name in one preview document, in bytes.
pub const MAX_PREVIEW_FIELD_NAME_BYTES: usize = 256;
/// Largest text or image value in one preview document, in bytes.
pub const MAX_PREVIEW_VALUE_BYTES: usize = 16 * 1024;
/// Largest preview document, in bytes.
pub const MAX_PREVIEW_DOCUMENT_BYTES: usize = 256 * 1024;

/// A preview state's live values, revision, and policy, ready to persist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewSnapshot {
    pub policy: StagePolicy,
    pub revision: u64,
    pub live: BTreeMap<String, BoundValue>,
}

/// Why a preview document or snapshot is invalid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreviewDocumentError {
    TooLarge { maximum: usize },
    Version { found: String },
    Policy { found: String },
    Revision { found: String },
    Line { index: usize },
    DuplicateField { field: String },
    TooManyFields { maximum: usize },
    FieldNameTooLong { maximum: usize },
    ValueTooLong { maximum: usize },
    Kind { found: String },
    Value { found: String },
}

impl fmt::Display for PreviewDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { maximum } => {
                write!(formatter, "preview document exceeds {maximum} bytes")
            }
            Self::Version { found } => {
                write!(formatter, "preview document version {found:?} is not 1")
            }
            Self::Policy { found } => write!(formatter, "unknown preview policy {found:?}"),
            Self::Revision { found } => write!(formatter, "invalid preview revision {found:?}"),
            Self::Line { index } => write!(formatter, "preview line {index} is malformed"),
            Self::DuplicateField { field } => {
                write!(formatter, "preview field {field:?} appears more than once")
            }
            Self::TooManyFields { maximum } => {
                write!(formatter, "preview document exceeds {maximum} fields")
            }
            Self::FieldNameTooLong { maximum } => {
                write!(formatter, "preview field name exceeds {maximum} bytes")
            }
            Self::ValueTooLong { maximum } => {
                write!(formatter, "preview value exceeds {maximum} bytes")
            }
            Self::Kind { found } => write!(formatter, "unknown preview field kind {found:?}"),
            Self::Value { found } => write!(formatter, "invalid preview value {found:?}"),
        }
    }
}

impl std::error::Error for PreviewDocumentError {}

impl PreviewSnapshot {
    /// Validates the snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewDocumentError`] for too many fields, an empty or
    /// oversized name, or an oversized text or image value.
    pub fn validate(&self) -> Result<(), PreviewDocumentError> {
        if self.live.len() > MAX_PREVIEW_FIELDS {
            return Err(PreviewDocumentError::TooManyFields {
                maximum: MAX_PREVIEW_FIELDS,
            });
        }
        for (field, value) in &self.live {
            if field.is_empty() || field.len() > MAX_PREVIEW_FIELD_NAME_BYTES {
                return Err(PreviewDocumentError::FieldNameTooLong {
                    maximum: MAX_PREVIEW_FIELD_NAME_BYTES,
                });
            }
            if let BoundValue::Text(text) | BoundValue::Image(text) = value
                && text.len() > MAX_PREVIEW_VALUE_BYTES
            {
                return Err(PreviewDocumentError::ValueTooLong {
                    maximum: MAX_PREVIEW_VALUE_BYTES,
                });
            }
        }
        Ok(())
    }

    /// Serializes the snapshot as a strict, versioned text document.
    #[must_use]
    pub fn to_document(&self) -> String {
        use fmt::Write as _;
        let mut output = String::from("freemix-data-preview 1\n");
        output.push_str(match self.policy {
            StagePolicy::RequireComplete => "policy require_complete\n",
            StagePolicy::KeepLiveOnError => "policy keep_live_on_error\n",
        });
        let _ = writeln!(output, "revision {}", self.revision);
        for (field, value) in &self.live {
            let (kind, value) = match value {
                BoundValue::Text(text) => ("text", encode_preview_text(text)),
                BoundValue::Number(number) => ("number", number.to_string()),
                BoundValue::Color(color) => (
                    "color",
                    format!(
                        "#{:02x}{:02x}{:02x}{:02x}",
                        color.r, color.g, color.b, color.a
                    ),
                ),
                BoundValue::Image(uri) => ("image", encode_preview_text(uri)),
                BoundValue::Visible(visible) => ("visibility", visible.to_string()),
            };
            let _ = writeln!(output, "live {} {kind} {value}", encode_preview_text(field));
        }
        output
    }

    /// Parses a strict preview document.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewDocumentError`] for an oversized, malformed, or
    /// unknown document without accepting a partial result.
    pub fn from_document(document: &str) -> Result<Self, PreviewDocumentError> {
        if document.len() > MAX_PREVIEW_DOCUMENT_BYTES {
            return Err(PreviewDocumentError::TooLarge {
                maximum: MAX_PREVIEW_DOCUMENT_BYTES,
            });
        }
        let mut lines = document.lines().enumerate();
        let Some((_, header)) = lines.next() else {
            return Err(PreviewDocumentError::Line { index: 1 });
        };
        if header != "freemix-data-preview 1" {
            return Err(PreviewDocumentError::Version {
                found: header.to_owned(),
            });
        }
        let mut policy = None;
        let mut revision = None;
        let mut live = BTreeMap::new();
        for (index, line) in lines {
            let number = index + 1;
            if line.is_empty() {
                return Err(PreviewDocumentError::Line { index: number });
            }
            if let Some(found) = line.strip_prefix("policy ") {
                policy = Some(match found {
                    "require_complete" => StagePolicy::RequireComplete,
                    "keep_live_on_error" => StagePolicy::KeepLiveOnError,
                    _ => {
                        return Err(PreviewDocumentError::Policy {
                            found: found.to_owned(),
                        });
                    }
                });
                continue;
            }
            if let Some(found) = line.strip_prefix("revision ") {
                revision = Some(found.parse().map_err(|_| PreviewDocumentError::Revision {
                    found: found.to_owned(),
                })?);
                continue;
            }
            let Some(rest) = line.strip_prefix("live ") else {
                return Err(PreviewDocumentError::Line { index: number });
            };
            let mut parts = rest.splitn(3, ' ');
            let (Some(field), Some(kind), Some(value)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(PreviewDocumentError::Line { index: number });
            };
            let field =
                decode_preview_text(field).ok_or(PreviewDocumentError::Line { index: number })?;
            if field.is_empty() || field.len() > MAX_PREVIEW_FIELD_NAME_BYTES {
                return Err(PreviewDocumentError::FieldNameTooLong {
                    maximum: MAX_PREVIEW_FIELD_NAME_BYTES,
                });
            }
            if live.contains_key(&field) {
                return Err(PreviewDocumentError::DuplicateField { field });
            }
            if live.len() >= MAX_PREVIEW_FIELDS {
                return Err(PreviewDocumentError::TooManyFields {
                    maximum: MAX_PREVIEW_FIELDS,
                });
            }
            let decoded =
                decode_preview_text(value).ok_or(PreviewDocumentError::Line { index: number })?;
            let parsed = match kind {
                "text" => BoundValue::Text(decoded),
                "number" => BoundValue::Number(decoded.parse().map_err(|_| {
                    PreviewDocumentError::Value {
                        found: decoded.clone(),
                    }
                })?),
                "color" => BoundValue::Color(parse_preview_color(&decoded).ok_or(
                    PreviewDocumentError::Value {
                        found: decoded.clone(),
                    },
                )?),
                "image" => BoundValue::Image(decoded),
                "visibility" => BoundValue::Visible(match decoded.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return Err(PreviewDocumentError::Value { found: decoded });
                    }
                }),
                _ => {
                    return Err(PreviewDocumentError::Kind {
                        found: kind.to_owned(),
                    });
                }
            };
            live.insert(field, parsed);
        }
        let snapshot = Self {
            policy: policy.ok_or(PreviewDocumentError::Line { index: 2 })?,
            revision: revision.ok_or(PreviewDocumentError::Line { index: 3 })?,
            live,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
}

fn encode_preview_text(text: &str) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'~') {
            output.push(char::from(byte));
        } else {
            let _ = write!(output, "%{byte:02X}");
        }
    }
    output
}

fn decode_preview_text(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            let pair = [high, low];
            let pair = std::str::from_utf8(&pair).ok()?;
            output.push(u8::from_str_radix(pair, 16).ok()?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn parse_preview_color(text: &str) -> Option<crate::TitleColor> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 8 {
        return None;
    }
    let channel = |index: usize| u8::from_str_radix(&hex[index..index + 2], 16).ok();
    Some(crate::TitleColor {
        r: channel(0)?,
        g: channel(2)?,
        b: channel(4)?,
        a: channel(6)?,
    })
}

impl PreviewState {
    /// The state's live values, revision, and policy.
    #[must_use]
    pub fn snapshot(&self) -> PreviewSnapshot {
        PreviewSnapshot {
            policy: self.policy,
            revision: self.revision,
            live: self.live.clone(),
        }
    }

    /// Restores live values and a revision without a staged preview.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewDocumentError`] when the snapshot is invalid.
    pub fn restore(snapshot: PreviewSnapshot) -> Result<Self, PreviewDocumentError> {
        snapshot.validate()?;
        Ok(Self {
            live: snapshot.live,
            revision: snapshot.revision,
            staged: None,
            policy: snapshot.policy,
        })
    }
}

/// The result of an atomic commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitOutcome {
    pub revision: u64,
    pub changes: Vec<FieldChange>,
}

/// Live (on-air) values plus at most one staged preview.
///
/// The revision starts at 0 and increments only when a commit or direct
/// apply actually changes live values.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PreviewState {
    live: BTreeMap<String, BoundValue>,
    revision: u64,
    staged: Option<StagedPreview>,
    policy: StagePolicy,
}

impl PreviewState {
    #[must_use]
    pub fn new(policy: StagePolicy) -> Self {
        Self {
            policy,
            ..Self::default()
        }
    }

    #[must_use]
    pub const fn live(&self) -> &BTreeMap<String, BoundValue> {
        &self.live
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub const fn staged(&self) -> Option<&StagedPreview> {
        self.staged.as_ref()
    }

    #[must_use]
    pub const fn policy(&self) -> StagePolicy {
        self.policy
    }

    /// `true` when a stage exists and live moved since it was staged.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.staged
            .as_ref()
            .is_some_and(|staged| staged.base_revision != self.revision)
    }

    fn next_values(
        &self,
        report: &BindingReport,
    ) -> Result<BTreeMap<String, BoundValue>, PreviewError> {
        let errored: Vec<&str> = report.errors().map(|field| field.field.as_str()).collect();
        if errored.is_empty() {
            return Ok(report.values.clone());
        }
        match self.policy {
            StagePolicy::RequireComplete => Err(PreviewError::Incomplete {
                errors: errored.into_iter().map(str::to_owned).collect(),
            }),
            StagePolicy::KeepLiveOnError => {
                let mut next = report.values.clone();
                for field in errored {
                    match self.live.get(field) {
                        Some(previous) => {
                            next.insert(field.to_owned(), previous.clone());
                        }
                        None => {
                            next.remove(field);
                        }
                    }
                }
                Ok(next)
            }
        }
    }

    /// Stages `report`, replacing any previous stage, and returns its changes.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError::Incomplete`] under
    /// [`StagePolicy::RequireComplete`]; the previous stage is kept.
    pub fn stage(&mut self, report: BindingReport) -> Result<&StagedPreview, PreviewError> {
        let next = self.next_values(&report)?;
        let changes = diff_values(&self.live, &next);
        Ok(self.staged.insert(StagedPreview {
            base_revision: self.revision,
            report,
            next,
            changes,
        }))
    }

    /// Drops the staged preview, returning it.
    pub fn discard(&mut self) -> Option<StagedPreview> {
        self.staged.take()
    }

    /// Atomically replaces live with the staged values and clears the stage.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError::NothingStaged`] or [`PreviewError::Stale`];
    /// a stale stage is kept for inspection.
    pub fn commit(&mut self) -> Result<CommitOutcome, PreviewError> {
        let staged = self.staged.as_ref().ok_or(PreviewError::NothingStaged)?;
        if staged.base_revision != self.revision {
            return Err(PreviewError::Stale {
                staged: staged.base_revision,
                live: self.revision,
            });
        }
        let staged = self.staged.take().ok_or(PreviewError::NothingStaged)?;
        Ok(self.replace_live(staged.next, staged.changes))
    }

    /// Applies `report` straight to live without a preview (e.g. automatic
    /// data refresh). Any existing stage becomes stale.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError::Incomplete`] under
    /// [`StagePolicy::RequireComplete`].
    pub fn apply_direct(&mut self, report: &BindingReport) -> Result<CommitOutcome, PreviewError> {
        let next = self.next_values(report)?;
        let changes = diff_values(&self.live, &next);
        Ok(self.replace_live(next, changes))
    }

    fn replace_live(
        &mut self,
        next: BTreeMap<String, BoundValue>,
        changes: Vec<FieldChange>,
    ) -> CommitOutcome {
        if !changes.is_empty() {
            self.live = next;
            self.revision += 1;
        }
        CommitOutcome {
            revision: self.revision,
            changes,
        }
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn preview_snapshots_round_trip_through_a_strict_document() {
        let mut live = BTreeMap::new();
        live.insert("home name".into(), BoundValue::Text("Harbor City".into()));
        live.insert("score".into(), BoundValue::Number(5));
        live.insert(
            "color".into(),
            BoundValue::Color(crate::TitleColor {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            }),
        );
        live.insert("logo".into(), BoundValue::Image("asset://logo.png".into()));
        live.insert("visible".into(), BoundValue::Visible(true));
        let snapshot = PreviewSnapshot {
            policy: StagePolicy::KeepLiveOnError,
            revision: 7,
            live,
        };
        let document = snapshot.to_document();
        assert_eq!(
            PreviewSnapshot::from_document(&document),
            Ok(snapshot.clone())
        );
        let state = PreviewState::restore(snapshot).unwrap();
        assert_eq!(state.revision(), 7);
        assert_eq!(state.live().len(), 5);

        for document in [
            "freemix-data-preview 2\n",
            "freemix-data-preview 1\npolicy require_complete\nrevision 0\nlive a text %ZZ\n",
            "freemix-data-preview 1\npolicy require_complete\nrevision 0\nlive a text x\nlive a text y\n",
            "freemix-data-preview 1\npolicy nope\nrevision 0\n",
            "freemix-data-preview 1\npolicy require_complete\nrevision x\n",
        ] {
            assert!(
                PreviewSnapshot::from_document(document).is_err(),
                "{document:?}"
            );
        }
    }
}
