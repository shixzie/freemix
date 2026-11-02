//! A bounded `OpenDocument` Spreadsheet reader (GX-004).
//!
//! [`parse_ods`] reads the first table of an `.ods` package and turns it into
//! an [`OdsRowsAdapter`]: the first non-empty row supplies the header names
//! and every later non-empty row becomes one object. Text cells keep their
//! paragraphs (joined with a newline), float, currency, percentage, date, and
//! time cells keep the value the document stored, and boolean cells become
//! `TRUE`/`FALSE`.
//!
//! The reader is deliberately narrow and bounded. It reads only `content.xml`,
//! and the package, the part, rows, columns, one cell's text, and repeated
//! rows or columns all have hard caps. A malformed package, a missing table, a
//! duplicate header, or a data cell under a blank header is refused rather
//! than guessed at.

use std::{
    collections::BTreeMap,
    fmt,
    io::{Cursor, Read},
};

use zip::ZipArchive;

use crate::adapters::{AdapterError, DataAdapter};
use crate::parse::{ParseError, ParseLimits};
use crate::value::DataValue;
use crate::xml::parse_xml;
use crate::xpath::{child_elements, element_attribute_map, element_tag};

/// Largest ODS package accepted, in bytes.
pub const MAX_ODS_BYTES: usize = 32 * 1024 * 1024;
/// Largest inflated `content.xml` accepted, in bytes.
pub const MAX_ODS_CONTENT_BYTES: u64 = 16 * 1024 * 1024;
/// Most non-empty rows read, including the header row.
pub const MAX_ODS_ROWS: usize = 50_000;
/// Most columns one table may name.
pub const MAX_ODS_COLUMNS: usize = 256;
/// Longest text one cell may carry, in UTF-8 bytes.
pub const MAX_ODS_CELL_BYTES: usize = 64 * 1024;
/// Longest table name accepted, in UTF-8 bytes.
pub const MAX_ODS_SHEET_NAME_BYTES: usize = 128;

/// Why an ODS workbook or table could not be read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OdsError {
    /// The package is larger than [`MAX_ODS_BYTES`].
    PackageTooLarge { actual: usize, maximum: usize },
    /// The bytes are not a readable ZIP package.
    NotPackage,
    /// `content.xml` is absent.
    MissingContent,
    /// `content.xml` inflates past its bound.
    ContentTooLarge,
    /// `content.xml` is not valid XML.
    Xml(ParseError),
    /// The package has no spreadsheet body.
    MissingSpreadsheet,
    /// The package lists no table.
    NoSheets,
    /// A requested table name is absent.
    UnknownSheet(String),
    /// A repeated row or column count is zero or malformed.
    InvalidRepeat(String),
    /// A boolean cell is neither `true` nor `false`.
    InvalidBoolean(String),
    /// One cell's text is too long.
    CellTooLong { row: usize, column: usize },
    /// A table names too many columns.
    TooManyColumns { maximum: usize },
    /// A worksheet has too many non-empty rows.
    TooManyRows { maximum: usize },
    /// The first non-empty row has no named column.
    MissingHeaders,
    /// Two header cells carry the same name.
    DuplicateHeader(String),
    /// A data cell sits under a blank header.
    UnlabelledColumn { row: usize, column: usize },
}

impl fmt::Display for OdsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PackageTooLarge { actual, maximum } => {
                write!(
                    formatter,
                    "spreadsheet is {actual} bytes; limit is {maximum}"
                )
            }
            Self::NotPackage => formatter.write_str("spreadsheet is not a ZIP package"),
            Self::MissingContent => formatter.write_str("spreadsheet has no content.xml"),
            Self::ContentTooLarge => formatter.write_str("content.xml inflates past its bound"),
            Self::Xml(error) => write!(formatter, "content.xml is not valid XML: {error}"),
            Self::MissingSpreadsheet => formatter.write_str("package has no spreadsheet body"),
            Self::NoSheets => formatter.write_str("package lists no table"),
            Self::UnknownSheet(name) => write!(formatter, "package has no table named {name}"),
            Self::InvalidRepeat(value) => {
                write!(formatter, "repeated row or column count {value} is invalid")
            }
            Self::InvalidBoolean(value) => {
                write!(formatter, "boolean cell value {value} is not true or false")
            }
            Self::CellTooLong { row, column } => write!(
                formatter,
                "cell at row {row} column {column} is longer than {MAX_ODS_CELL_BYTES} bytes"
            ),
            Self::TooManyColumns { maximum } => {
                write!(formatter, "table names more than {maximum} columns")
            }
            Self::TooManyRows { maximum } => {
                write!(formatter, "table has more than {maximum} non-empty rows")
            }
            Self::MissingHeaders => formatter.write_str("table has no named columns"),
            Self::DuplicateHeader(header) => write!(formatter, "duplicate header `{header}`"),
            Self::UnlabelledColumn { row, column } => {
                write!(formatter, "cell at row {row} column {column} has no header")
            }
        }
    }
}

impl std::error::Error for OdsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Xml(error) => Some(error),
            _ => None,
        }
    }
}

/// A parsed table: named columns and text rows for [`DataAdapter`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OdsRowsAdapter {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl OdsRowsAdapter {
    /// The header names in sheet column order, blank columns removed.
    #[must_use]
    pub fn headers(&self) -> &[String] {
        &self.headers
    }

    /// The data rows, each padded to the header count.
    #[must_use]
    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }
}

impl DataAdapter for OdsRowsAdapter {
    fn representation(&self) -> Result<DataValue, AdapterError> {
        let mut output = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let object = self
                .headers
                .iter()
                .cloned()
                .zip(row.iter().cloned().map(DataValue::String))
                .collect::<BTreeMap<_, _>>();
            output.push(DataValue::Object(object));
        }
        Ok(DataValue::List(output))
    }
}

/// Reads the first table of an `OpenDocument` Spreadsheet.
///
/// # Errors
///
/// Returns [`OdsError`] for a non-ZIP package, a missing or oversized
/// `content.xml`, a malformed table, or a violated row, column, repeat, or
/// cell bound.
pub fn parse_ods(bytes: &[u8]) -> Result<OdsRowsAdapter, OdsError> {
    parse_workbook(bytes, None)
}

/// Reads one named table of an `OpenDocument` Spreadsheet.
///
/// # Errors
///
/// Returns [`OdsError`] for a non-ZIP package, an unknown table, a missing or
/// oversized `content.xml`, a malformed table, or a violated row, column,
/// repeat, or cell bound.
pub fn parse_ods_sheet(bytes: &[u8], sheet_name: &str) -> Result<OdsRowsAdapter, OdsError> {
    parse_workbook(bytes, Some(sheet_name))
}

fn parse_workbook(bytes: &[u8], selected: Option<&str>) -> Result<OdsRowsAdapter, OdsError> {
    if bytes.len() > MAX_ODS_BYTES {
        return Err(OdsError::PackageTooLarge {
            actual: bytes.len(),
            maximum: MAX_ODS_BYTES,
        });
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|_| OdsError::NotPackage)?;
    let content = {
        let Ok(file) = archive.by_name("content.xml") else {
            return Err(OdsError::MissingContent);
        };
        let mut bytes = Vec::new();
        file.take(MAX_ODS_CONTENT_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| OdsError::MissingContent)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ODS_CONTENT_BYTES {
            return Err(OdsError::ContentTooLarge);
        }
        bytes
    };
    let limits = ParseLimits {
        max_input_bytes: usize::try_from(MAX_ODS_CONTENT_BYTES).unwrap_or(usize::MAX),
        max_rows: MAX_ODS_ROWS,
        max_columns: MAX_ODS_COLUMNS,
        max_field_bytes: MAX_ODS_CELL_BYTES,
        max_depth: 32,
        max_nodes: 2_000_000,
    };
    let document = parse_xml(&content, &limits).map_err(OdsError::Xml)?;
    let spreadsheet =
        descendant(&document, "office:spreadsheet").ok_or(OdsError::MissingSpreadsheet)?;
    let tables: Vec<&DataValue> = child_elements(spreadsheet)
        .filter(|child| element_tag(child) == Some("table:table"))
        .collect();
    let table = match selected {
        Some(name) => tables
            .iter()
            .find(|table| attribute(table, "table:name") == Some(name))
            .copied()
            .ok_or_else(|| OdsError::UnknownSheet(name.to_owned()))?,
        None => tables.first().copied().ok_or(OdsError::NoSheets)?,
    };
    if let Some(name) = attribute(table, "table:name")
        && name.len() > MAX_ODS_SHEET_NAME_BYTES
    {
        return Err(OdsError::UnknownSheet(name.to_owned()));
    }
    let rows = table_rows(table)?;
    build_rows(&rows)
}

fn table_rows(table: &DataValue) -> Result<Vec<Vec<String>>, OdsError> {
    let mut row_nodes = Vec::new();
    collect_rows(table, &mut row_nodes);
    let mut rows = Vec::new();
    for row in row_nodes {
        let repeat = count_attribute(row, "table:number-rows-repeated")?;
        let cells = row_cells(row)?;
        if cells.iter().all(String::is_empty) {
            continue;
        }
        let total = rows.len().saturating_add(repeat);
        if total > MAX_ODS_ROWS {
            return Err(OdsError::TooManyRows {
                maximum: MAX_ODS_ROWS,
            });
        }
        for _ in 0..repeat {
            rows.push(cells.clone());
        }
    }
    Ok(rows)
}

fn collect_rows<'a>(table: &'a DataValue, out: &mut Vec<&'a DataValue>) {
    for child in child_elements(table) {
        match element_tag(child) {
            Some("table:table-row") => out.push(child),
            Some("table:table-header-rows" | "table:table-rows" | "table:table-row-group") => {
                collect_rows(child, out);
            }
            _ => {}
        }
    }
}

fn row_cells(row: &DataValue) -> Result<Vec<String>, OdsError> {
    let mut cells: Vec<Option<String>> = Vec::new();
    let mut column = 0usize;
    for child in child_elements(row) {
        let tag = element_tag(child);
        let repeat = count_attribute(child, "table:number-columns-repeated")?;
        match tag {
            Some("table:table-cell") => {
                let value = cell_value(child)?;
                if value.len() > MAX_ODS_CELL_BYTES {
                    return Err(OdsError::CellTooLong {
                        row: 1,
                        column: column + 1,
                    });
                }
                if value.is_empty() {
                    column = column.saturating_add(repeat);
                    continue;
                }
                if column >= MAX_ODS_COLUMNS || column.saturating_add(repeat) > MAX_ODS_COLUMNS {
                    return Err(OdsError::TooManyColumns {
                        maximum: MAX_ODS_COLUMNS,
                    });
                }
                for _ in 0..repeat {
                    if cells.len() <= column {
                        cells.resize(column + 1, None);
                    }
                    cells[column] = Some(value.clone());
                    column += 1;
                }
            }
            Some("table:covered-table-cell") => {
                column = column.saturating_add(repeat);
            }
            _ => {}
        }
    }
    Ok(cells.into_iter().map(Option::unwrap_or_default).collect())
}

fn cell_value(cell: &DataValue) -> Result<String, OdsError> {
    let kind = attribute(cell, "office:value-type").unwrap_or("");
    match kind {
        "boolean" => match attribute(cell, "office:boolean-value") {
            Some("true") => Ok("TRUE".to_owned()),
            Some("false") => Ok("FALSE".to_owned()),
            Some(other) => Err(OdsError::InvalidBoolean(other.to_owned())),
            None => Ok(cell_text(cell)),
        },
        "date" => {
            Ok(attribute(cell, "office:date-value").map_or_else(|| cell_text(cell), str::to_owned))
        }
        "time" => {
            Ok(attribute(cell, "office:time-value").map_or_else(|| cell_text(cell), str::to_owned))
        }
        "float" | "currency" | "percentage" => {
            Ok(attribute(cell, "office:value").map_or_else(|| cell_text(cell), str::to_owned))
        }
        _ => Ok(cell_text(cell)),
    }
}

/// The cell's text: direct paragraphs or headings joined by a newline.
fn cell_text(cell: &DataValue) -> String {
    let mut paragraphs = Vec::new();
    for child in child_elements(cell) {
        if matches!(element_tag(child), Some("text:p" | "text:h")) {
            paragraphs.push(paragraph_text(child));
        }
    }
    if paragraphs.is_empty() {
        paragraph_text(cell)
    } else {
        paragraphs.join("\n")
    }
}

fn paragraph_text(node: &DataValue) -> String {
    let mut text = String::new();
    collect_text(node, &mut text);
    text
}

fn collect_text(node: &DataValue, out: &mut String) {
    let DataValue::Object(fields) = node else {
        return;
    };
    let Some(DataValue::List(children)) = fields.get("children") else {
        return;
    };
    for child in children {
        match child {
            DataValue::String(text) => out.push_str(text),
            other => match element_tag(other) {
                Some("text:s") => {
                    let count = attribute(other, "text:c")
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(1)
                        .min(1_000);
                    out.extend(std::iter::repeat_n(' ', count));
                }
                Some("text:tab") => out.push('\t'),
                Some("text:line-break") => out.push('\n'),
                _ => collect_text(other, out),
            },
        }
    }
}

fn build_rows(rows: &[Vec<String>]) -> Result<OdsRowsAdapter, OdsError> {
    let header = rows.first().ok_or(OdsError::MissingHeaders)?;
    let mut columns = Vec::new();
    for (column, value) in header.iter().enumerate() {
        let name = value.trim();
        if name.is_empty() {
            continue;
        }
        if name.len() > MAX_ODS_CELL_BYTES {
            return Err(OdsError::CellTooLong {
                row: 1,
                column: column + 1,
            });
        }
        if columns.iter().any(|(_, existing)| existing == name) {
            return Err(OdsError::DuplicateHeader(name.to_owned()));
        }
        columns.push((column, name.to_owned()));
    }
    if columns.is_empty() {
        return Err(OdsError::MissingHeaders);
    }
    let mut output = Vec::with_capacity(rows.len().saturating_sub(1));
    for (index, row) in rows.iter().enumerate().skip(1) {
        for (column, value) in row.iter().enumerate() {
            if !value.is_empty() && !columns.iter().any(|(named, _)| *named == column) {
                return Err(OdsError::UnlabelledColumn {
                    row: index + 1,
                    column: column + 1,
                });
            }
        }
        output.push(
            columns
                .iter()
                .map(|(column, _)| row.get(*column).cloned().unwrap_or_default())
                .collect(),
        );
    }
    Ok(OdsRowsAdapter {
        headers: columns.into_iter().map(|(_, name)| name).collect(),
        rows: output,
    })
}

fn count_attribute(node: &DataValue, name: &str) -> Result<usize, OdsError> {
    match attribute(node, name) {
        None => Ok(1),
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|count| *count >= 1)
            .ok_or_else(|| OdsError::InvalidRepeat(value.to_owned())),
    }
}

fn descendant<'a>(node: &'a DataValue, name: &str) -> Option<&'a DataValue> {
    for child in child_elements(node) {
        if element_tag(child) == Some(name) {
            return Some(child);
        }
        if let Some(found) = descendant(child, name) {
            return Some(found);
        }
    }
    None
}

fn attribute<'a>(node: &'a DataValue, name: &str) -> Option<&'a str> {
    match element_attribute_map(node)?.get(name)? {
        DataValue::String(value) => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    fn package(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, contents) in parts {
            writer
                .start_file(
                    (*name).to_owned(),
                    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    const CONTENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
        <office:document-content
          xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
          xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
          xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
          <office:body><office:spreadsheet>
            <table:table table:name="Data">
              <table:table-header-rows>
                <table:table-row>
                  <table:table-cell office:value-type="string"><text:p>Name</text:p></table:table-cell>
                  <table:table-cell office:value-type="string"><text:p>Score</text:p></table:table-cell>
                  <table:table-cell office:value-type="string"><text:p>Active</text:p></table:table-cell>
                  <table:table-cell office:value-type="string"><text:p>Date</text:p></table:table-cell>
                </table:table-row>
              </table:table-header-rows>
              <table:table-row table:number-rows-repeated="2">
                <table:table-cell office:value-type="string"><text:p>Ada</text:p></table:table-cell>
                <table:table-cell office:value-type="float" office:value="42.5"/>
                <table:table-cell office:value-type="boolean" office:boolean-value="true"/>
              </table:table-row>
              <table:table-row>
                <table:table-cell office:value-type="string"><text:p>Text <text:span>run</text:span></text:p></table:table-cell>
                <table:table-cell table:number-columns-repeated="2"/>
                <table:table-cell office:value-type="date" office:date-value="2024-01-02"/>
              </table:table-row>
            </table:table>
            <table:table table:name="Other">
              <table:table-row><table:table-cell office:value-type="string"><text:p>Label</text:p></table:table-cell></table:table-row>
              <table:table-row><table:table-cell office:value-type="string"><text:p>Other</text:p></table:table-cell></table:table-row>
            </table:table>
          </office:spreadsheet></office:body>
        </office:document-content>"#;

    fn workbook() -> Vec<u8> {
        package(&[
            ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
            ("content.xml", CONTENT),
        ])
    }

    #[test]
    fn an_ods_table_becomes_header_keyed_text_rows() {
        let adapter = parse_ods(&workbook()).unwrap();
        assert_eq!(adapter.headers(), ["Name", "Score", "Active", "Date"]);
        assert_eq!(
            adapter.rows(),
            [
                vec![
                    "Ada".to_owned(),
                    "42.5".to_owned(),
                    "TRUE".to_owned(),
                    String::new()
                ],
                vec![
                    "Ada".to_owned(),
                    "42.5".to_owned(),
                    "TRUE".to_owned(),
                    String::new()
                ],
                vec![
                    "Text run".to_owned(),
                    String::new(),
                    String::new(),
                    "2024-01-02".to_owned()
                ],
            ]
        );
    }

    #[test]
    fn a_named_ods_table_is_read() {
        let adapter = parse_ods_sheet(&workbook(), "Other").unwrap();
        assert_eq!(adapter.headers(), ["Label"]);
        assert_eq!(adapter.rows(), [vec!["Other".to_owned()]]);
        assert_eq!(
            parse_ods_sheet(&workbook(), "Missing"),
            Err(OdsError::UnknownSheet("Missing".to_owned()))
        );
    }

    #[test]
    fn malformed_ods_packages_are_refused() {
        assert_eq!(parse_ods(b"not a zip"), Err(OdsError::NotPackage));
        let duplicate = package(&[(
            "content.xml",
            r#"<office:document-content xmlns:office="o" xmlns:table="t" xmlns:text="x"><office:body><office:spreadsheet><table:table><table:table-row><table:table-cell office:value-type="string"><text:p>Name</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>Name</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#,
        )]);
        assert_eq!(
            parse_ods(&duplicate),
            Err(OdsError::DuplicateHeader("Name".to_owned()))
        );
    }
}
