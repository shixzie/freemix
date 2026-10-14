//! Durable, engine-independent `FreeMix` project manifests.

mod append;
mod asset;
mod autosave;
mod bundle;
mod fsutil;
mod journal;
mod journal_db;
mod json;
mod lut;
mod media;
mod model;
mod recent;
mod settings_export;
mod settings_import;
mod store;
mod title_template;

pub use append::{
    AppendReport, AppendedAsset, AppendedAssetDisposition, RenamedEntity, SkippedContent,
};
pub use asset::{AssetAuditIssue, AssetAuditReason, AssetResolveError, validate_asset_uri};
pub use autosave::{
    AutosaveError, AutosaveGeneration, AutosaveRecovery, AutosaveRejection, AutosaveWrite,
    DEFAULT_AUTOSAVE_GENERATIONS, MAX_AUTOSAVE_GENERATIONS,
};
pub use bundle::{BundleExport, BundledAsset};
pub use journal::{
    AbandonedJournal, CompactionReport, JournalError, JournalObservation, JournalProgress,
    JournalScan, JournalWriter, MAX_JOURNAL_PROGRESS_BYTES, MAX_JOURNAL_RECORD_BYTES,
    MAX_UNAPPLIED_JOURNAL_BATCHES, MAX_UNAPPLIED_JOURNAL_BYTES, MutationBatch,
};
pub use lut::{ImportedLut, LutError};
pub use media::{
    AssetDisposition, BundleError, ImportedAsset, RelinkEntry, RelinkOutcome, RelinkPlan,
    RelinkResolution, RelinkSearch,
};
pub use model::{
    CURRENT_SCHEMA_VERSION, FadeToBlackState, IdempotencyReceipt,
    MAX_OVERLAY_TRANSITION_DURATION_FRAMES, ManualTransitionKind, ManualTransitionState,
    OVERLAY_CHANNEL_COUNT, ProjectPosition, ProjectValidationError, ReceiptOutcome, ReferenceField,
    RuntimeFadeToBlack, RuntimeManualTransitions, RuntimeOverlayBorder, RuntimeOverlayChannel,
    RuntimeOverlayPosition, RuntimeOverlayTransition, RuntimeOverlays, RuntimeRouting,
    StoredProject,
};
pub use recent::{
    DEFAULT_RECENT_PROJECTS, MAX_RECENT_PROJECTS, MAX_RECENT_PROJECTS_BYTES,
    RECENT_PROJECTS_CONTRACT, RecentProject, RecentProjectStatus, RecentProjects,
    RecentProjectsError,
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
pub use title_template::{
    ImportedTitleFont, ImportedTitleTemplate, MAX_TITLE_TEMPLATE_BYTES, StoredTitleTemplate,
    TITLE_TEMPLATE_EXTENSION, TitleFontAsset, TitleImportError, TitleIssue, TitleProblem,
    TitleTemplateError, decode_title_template, encode_title_template, field_type,
    title_field_defaults, title_field_value,
};
