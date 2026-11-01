// Public only so the daemon binary can use these items; this crate is not a
// stable API surface, so the public-API documentation and must-use lints the
// rest of the workspace applies to libraries are not useful here.
#![allow(
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::return_self_not_must_use
)]

//! The daemon's durable store: the mutation journal, checkpoints, progress
//! markers, document assets shared with replay, and the errors those paths
//! report.
//!
//! This crate is a mechanical extraction from `freemixd`; the journal and
//! document modules keep their original contracts.

use std::{error::Error, fmt};

use fm_command::RejectionCode;
use fm_model::LutId;
use fm_persistence::{LutError, ProjectStore};
use fm_protocol::{CommandMessage, CommandPayload, RefusalReason};

pub mod documents;
mod journal;

pub use journal::*;

/// A daemon-side failure whose message is already operator-facing.
pub type AppResult<T> = Result<T, Box<dyn Error>>;

/// A failure with a stable, already-worded message.
#[derive(Debug)]
pub struct AppFailure(pub String);

impl fmt::Display for AppFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for AppFailure {}

/// A command the daemon refused instead of failing the show.
#[derive(Debug)]
pub struct CommandRefusal {
    pub code: RejectionCode,
    pub retryable: bool,
    pub message: String,
    /// The typed reason clients word in their language (Protocol 2.45).
    pub reason: Option<RefusalReason>,
}

impl fmt::Display for CommandRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl Error for CommandRefusal {}

/// Writes the LUT an accepted `grade_lut_import` carries into the bundle.
///
/// Called before the command's journal batch is appended, so a crash after
/// the append always finds the file; a crash before it leaves an unreferenced
/// file.
///
/// # Errors
///
/// Returns the bundle's typed LUT write failure.
pub fn write_imported_lut(store: &ProjectStore, command: &CommandMessage) -> Result<(), LutError> {
    let CommandPayload::ImportGradeLut { lut, data, .. } = &command.payload else {
        return Ok(());
    };
    store.write_lut_source(LutId::new(*lut), data)
}
