//! Durable, engine-independent `FreeMix` project manifests.

mod asset;
mod journal;
mod journal_db;
mod json;
mod model;
mod settings_export;
mod settings_import;
mod store;

pub use asset::{AssetAuditIssue, AssetAuditReason, AssetResolveError, validate_asset_uri};
pub use journal::{
    AbandonedJournal, CompactionReport, JournalError, JournalObservation, JournalScan,
    JournalWriter, MAX_JOURNAL_RECORD_BYTES, MAX_UNAPPLIED_JOURNAL_BATCHES,
    MAX_UNAPPLIED_JOURNAL_BYTES, MutationBatch,
};
pub use model::{
    CURRENT_SCHEMA_VERSION, FadeToBlackState, IdempotencyReceipt,
    MAX_OVERLAY_TRANSITION_DURATION_FRAMES, ManualTransitionKind, ManualTransitionState,
    OVERLAY_CHANNEL_COUNT, ProjectPosition, ProjectValidationError, ReceiptOutcome, ReferenceField,
    RuntimeFadeToBlack, RuntimeManualTransitions, RuntimeOverlayBorder, RuntimeOverlayChannel,
    RuntimeOverlayPosition, RuntimeOverlayTransition, RuntimeOverlays, RuntimeRouting,
    StoredProject,
};
pub use settings_export::{
    EntityKey, MAX_SECRET_BYTES, MAX_SETTING_KEY_BYTES, MAX_SETTING_KEY_SEGMENT_BYTES,
    MAX_SETTING_KEY_SEGMENTS, MAX_SETTING_TEXT_BYTES, MAX_SETTINGS_ENTRIES,
    MAX_SETTINGS_EXPORT_BYTES, ReferenceKind, SECRET_FIELD_NAMES, SETTINGS_EXPORT_CONTRACT, Secret,
    SecretError, SecretSlot, SettingKey, SettingKeyError, SettingReference, SettingValue,
    SettingsExportError, SettingsSection, SettingsSnapshot,
};
pub use settings_import::{
    ChangeKind, ChangePreview, ContentInventory, ImportConflict, ImportError, ImportOutcome,
    ImportPlan, ImportResolutions, KeyChange, MergeStrategy, SectionChoice,
};
pub use store::{MAX_MANIFEST_BYTES, ProjectStore, StoreError};
