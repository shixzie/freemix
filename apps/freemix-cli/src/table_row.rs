//! Shared table-row reading for `title-apply-row` and `crawl-apply-table`.
//!
//! A row source is a flat JSON object, a CSV/TSV table, or the first or named
//! worksheet/table of an `.xlsx` or `.ods` workbook. Table rows become
//! `DataValue::Object`s keyed by their header names.

use std::{collections::BTreeMap, ffi::OsStr, fs, path::Path};

use fm_data::{
    CsvOptions, DataAdapter, DataValue, ParseLimits, parse_csv, parse_json, parse_ods, parse_xlsx,
};

use crate::app::{AppFailure, AppResult};

/// Every data row of `path` keyed by its header names.
pub(crate) fn read_rows(path: &Path) -> AppResult<Vec<BTreeMap<String, DataValue>>> {
    let bytes = fs::read(path).map_err(|error| {
        AppFailure(format!("row {} could not be read: {error}", path.display()))
    })?;
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "json" => {
            let row = parse_json(&bytes, &ParseLimits::default())
                .map_err(|error| AppFailure(format!("row {}: {error}", path.display())))?;
            let DataValue::Object(row) = row else {
                return Err(AppFailure(format!(
                    "row {} must be a JSON object keyed by field name",
                    path.display()
                ))
                .into());
            };
            Ok(vec![row])
        }
        "csv" | "tsv" => {
            let options = CsvOptions {
                delimiter: if extension == "tsv" { b'\t' } else { b',' },
                ..CsvOptions::default()
            };
            let adapter = parse_csv(&bytes, &options)
                .map_err(|error| AppFailure(format!("row {}: {error}", path.display())))?;
            table_objects(
                adapter
                    .representation()
                    .map_err(|error| AppFailure(format!("row {}: {error}", path.display())))?,
                path,
            )
        }
        "xlsx" => table_objects(
            parse_xlsx(&bytes)
                .map_err(|error| AppFailure(format!("row {}: {error}", path.display())))?
                .representation()
                .map_err(|error| AppFailure(format!("row {}: {error}", path.display())))?,
            path,
        ),
        "ods" => table_objects(
            parse_ods(&bytes)
                .map_err(|error| AppFailure(format!("row {}: {error}", path.display())))?
                .representation()
                .map_err(|error| AppFailure(format!("row {}: {error}", path.display())))?,
            path,
        ),
        _ => Err(AppFailure(format!(
            "row {} must be .json, .csv, .tsv, .xlsx, or .ods",
            path.display()
        ))
        .into()),
    }
}

/// One 1-based data row of `path`.
pub(crate) fn read_row(path: &Path, row_number: u64) -> AppResult<BTreeMap<String, DataValue>> {
    if row_number == 0 {
        return Err(AppFailure("row number is 1-based and cannot be zero".into()).into());
    }
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension == "json" && row_number != 1 {
        return Err(AppFailure(format!(
            "row {} is a JSON object and has no row {row_number}",
            path.display()
        ))
        .into());
    }
    let rows = read_rows(path)?;
    let index = usize::try_from(row_number - 1).unwrap_or(usize::MAX);
    rows.into_iter().nth(index).ok_or_else(|| {
        AppFailure(format!(
            "row {} has no data row {row_number}",
            path.display()
        ))
        .into()
    })
}

fn table_objects(value: DataValue, path: &Path) -> AppResult<Vec<BTreeMap<String, DataValue>>> {
    let DataValue::List(rows) = value else {
        return Err(AppFailure(format!("row {} did not produce a table", path.display())).into());
    };
    rows.into_iter()
        .map(|row| {
            let DataValue::Object(row) = row else {
                return Err(AppFailure(format!(
                    "row {} did not produce table objects",
                    path.display()
                ))
                .into());
            };
            Ok(row)
        })
        .collect()
}
