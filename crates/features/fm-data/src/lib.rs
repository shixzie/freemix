//! Typed data sources, transforms, mapping, and cache.
//!
//! This crate deliberately contains contracts and deterministic in-memory
//! adapters only. Network clients, database drivers, and system clocks belong
//! in integration crates.

mod adapters;
mod cache;
mod mapping;
mod parse;
mod retry;
mod schema;
mod secret;
mod select;
mod source;
mod title;
mod transform;
mod value;

pub use adapters::{AdapterError, CsvRowsAdapter, DataAdapter, JsonObjectAdapter};
pub use cache::{BoundedCache, CacheError, Clock, ManualClock};
pub use mapping::{FieldBinding, FieldMappingReport, Mapper, MappingReport, MappingStatus};
pub use parse::{CsvOptions, ParseError, ParseErrorKind, ParseLimits, parse_csv, parse_json};
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

#[cfg(test)]
mod tests;
