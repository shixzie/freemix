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
mod ods;
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
mod xlsx;
mod xml;
mod xpath;

pub use adapters::{
    AdapterError, CsvRowsAdapter, DataAdapter, JsonObjectAdapter, MAX_XML_ROW_FIELDS,
    XmlRowsAdapter,
};
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
pub use ods::{
    MAX_ODS_BYTES, MAX_ODS_CELL_BYTES, MAX_ODS_COLUMNS, MAX_ODS_CONTENT_BYTES, MAX_ODS_ROWS,
    MAX_ODS_SHEET_NAME_BYTES, OdsError, OdsRowsAdapter, parse_ods, parse_ods_sheet,
};
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
pub use xlsx::{
    MAX_XLSX_BYTES, MAX_XLSX_CELL_BYTES, MAX_XLSX_COLUMNS, MAX_XLSX_INFLATED_BYTES,
    MAX_XLSX_PART_BYTES, MAX_XLSX_ROWS, MAX_XLSX_SHARED_STRINGS, XlsxError, XlsxRowsAdapter,
    parse_xlsx, parse_xlsx_sheet,
};
pub use xml::parse_xml;
pub use xpath::{
    MAX_XPATH_BYTES, MAX_XPATH_INDEX, MAX_XPATH_MATCHES, MAX_XPATH_STEPS, XPathError, select_xml,
};

#[cfg(test)]
mod tests;
