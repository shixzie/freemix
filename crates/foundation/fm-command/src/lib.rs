//! Transport-neutral command execution primitives.
//!
//! The crate separates durable command acceptance from runtime realization.
//! [`CommandState`] is a single-writer, idempotent state machine for durable
//! mutations. [`Lifecycle`] tracks what happens after a revision is accepted.

mod counters;
mod envelope;
mod fault;
mod lifecycle;
mod machine;
mod refusal;
mod refusal_token;
mod transaction;

pub use counters::{
    CounterOverflow, EventSequence, Revision, RuntimeGeneration, RuntimeSequence, StateEpoch,
};
pub use envelope::{
    AcceptedReceipt, CommandEnvelope, CommandId, CommandReceipt, Deadline, FieldIssue,
    IdempotencyKey, RejectedReceipt, Rejection, RejectionCode,
};
pub use fault::{FaultDetail, MAX_FAULT_MESSAGE_BYTES, bound_fault_message};
pub use lifecycle::{
    FailureDisposition, Lifecycle, LifecycleError, LifecycleEvent, LifecyclePhase, LifecycleRecord,
    ScheduledDomain,
};
pub use machine::{
    ApplyOutcome, CommandState, CommitCounter, CommitError, DurableEvent, Mutation, PrepareOutcome,
    PreparedCommit,
};
pub use refusal::{IdKind, InputUse, MAX_REFUSAL_FIELD_BYTES, RefusalReason, ScriptFailureKind};
pub use transaction::{MAX_TRANSACTION_COMMANDS, Transaction, TransactionError};
