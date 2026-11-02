//! `crawl-apply-table`: feeds one table column into a stored crawl's items
//! (GX-004/GX-003).
//!
//! The table may be CSV/TSV, `.xlsx`, or `.ods`. Each non-empty value of the
//! named column becomes one crawl item; the style of the item at the same
//! position is kept. A missing column, a non-text value, or an empty result
//! refuses the command without touching the bundle.

use std::path::Path;

use fm_data::DataValue;
use fm_titles::{CrawlSnapshot, MAX_CRAWL_ITEMS};

use crate::app::{AppFailure, AppResult, input_id, load_stored_project};
use crate::table_row::read_rows;
use fm_persistence::ProjectStore;

/// Applies `field` from every row of `table` to the crawl stored on `input`.
pub(crate) fn apply_table(path: &Path, input: u128, table: &Path, field: &str) -> AppResult<()> {
    if field.is_empty() {
        return Err(AppFailure("crawl table field cannot be empty".into()).into());
    }
    let rows = read_rows(table)?;
    let mut texts = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let value = row.get(field).ok_or_else(|| {
            AppFailure(format!(
                "row {} data row {} has no field `{field}`",
                table.display(),
                index + 1
            ))
        })?;
        let text = match value {
            DataValue::String(text) => text.clone(),
            DataValue::Integer(number) => number.to_string(),
            DataValue::Decimal(number) => number.to_string(),
            DataValue::Bool(value) => {
                if *value {
                    "TRUE".to_owned()
                } else {
                    "FALSE".to_owned()
                }
            }
            DataValue::Null => continue,
            _ => {
                return Err(AppFailure(format!(
                    "row {} data row {} field `{field}` is not a text value",
                    table.display(),
                    index + 1
                ))
                .into());
            }
        };
        if text.trim().is_empty() {
            continue;
        }
        texts.push(text);
        if texts.len() > MAX_CRAWL_ITEMS {
            return Err(
                AppFailure(format!("crawl table has more than {MAX_CRAWL_ITEMS} items")).into(),
            );
        }
    }
    if texts.is_empty() {
        return Err(AppFailure(format!(
            "row {} field `{field}` has no non-empty values",
            table.display()
        ))
        .into());
    }

    let stored = load_stored_project(path)?;
    let input = input_id(input)?;
    let document = stored
        .project()
        .crawl_document(input)
        .ok_or_else(|| AppFailure(format!("input {input} has no stored crawl")))?;
    let mut snapshot = CrawlSnapshot::from_document(document)
        .map_err(|error| AppFailure(format!("crawl-apply-table: {error}")))?;
    snapshot
        .set_item_texts(texts)
        .map_err(|error| AppFailure(format!("crawl-apply-table: {error}")))?;
    let canonical = snapshot.to_document();
    let mut project = stored.project().clone();
    project
        .set_crawl_document(input, canonical)
        .map_err(|error| AppFailure(format!("crawl-apply-table: {error}")))?;
    ProjectStore::new(path)?.save(&stored.with_project(project)?)?;
    println!(
        "crawl-apply-table: input={input} items={}",
        snapshot.items.len()
    );
    Ok(())
}
