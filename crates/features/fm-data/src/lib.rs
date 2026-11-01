//! Typed data sources, transforms, mapping, and cache.
//!
//! This crate deliberately contains contracts and deterministic in-memory
//! adapters only. Network clients, database drivers, and system clocks belong
//! in integration crates.

mod adapters;
mod bind;
mod cache;
mod feed;
mod file;
mod format;
mod html;
mod mapping;
mod parse;
mod preview;
mod retry;
mod schema;
mod secret;
mod select;
mod source;
mod title;
mod transform;
mod value;
mod xml;

pub use adapters::{AdapterError, CsvRowsAdapter, DataAdapter, JsonObjectAdapter};
pub use bind::{
    BindingConfigError, BindingError, BindingFieldReport, BindingKind, BindingReport, BindingSet,
    BindingStatus, BoundValue, BoundValueKind, CompareOp, DataBinding, ImageUriError,
    ImageUriPolicy, RowBindingError, TruthyRule, VisibilityError, VisibilityRule,
};
pub use cache::{BoundedCache, CacheError, Clock, ManualClock};
pub use feed::{FeedError, parse_feed};
pub use file::{FileFormat, FilePollingSource, MAX_FILE_SOURCE_BYTES};
pub use format::{
    DateInput, DatePattern, DateTimeFormat, DecimalRounding, FormatError, NumberFormat, SignStyle,
    Timestamp, UtcOffset,
};
pub use html::parse_html_table;
pub use mapping::{FieldBinding, FieldMappingReport, Mapper, MappingReport, MappingStatus};
pub use parse::{CsvOptions, ParseError, ParseErrorKind, ParseLimits, parse_csv, parse_json};
pub use preview::{
    CommitOutcome, FieldChange, MAX_PREVIEW_DOCUMENT_BYTES, MAX_PREVIEW_FIELD_NAME_BYTES,
    MAX_PREVIEW_FIELDS, MAX_PREVIEW_VALUE_BYTES, PreviewDocumentError, PreviewError,
    PreviewSnapshot, PreviewState, StagePolicy, StagedPreview, diff_values,
};
pub use retry::{RetryError, RetryOutcome, RetryPolicy, RetryRecord, RetryState};
pub use schema::{
    DataPath, PathError, PathSegment, Schema, SchemaExtractError, SchemaType, TypeError,
};
pub use secret::SecretRefId;
pub use select::{EdgeBehavior, RowCursor, RowNavigation, RowSelectionError, rows_at};
pub use source::{
    DataSource, FakePollingSource, FakePushSource, PollingSource, PushSource, SourceError,
    SourceEvent, SourceState,
};
pub use title::{
    CoercionError, CoercionPolicy, NumberRounding, TitleColor, TitleFieldBinding, TitleFieldKind,
    TitleFieldReport, TitleFieldStatus, TitleFieldValue, TitleMapper, TitleMapperError,
    TitleMappingReport, coerce_title_value,
};
pub use transform::{Transform, TransformError, ValueSelector};
pub use value::{DataValue, Decimal, DecimalError, ValueType};
pub use xml::parse_xml;

#[cfg(test)]
mod tests;
