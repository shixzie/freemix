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
