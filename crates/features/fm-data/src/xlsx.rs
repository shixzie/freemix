//! A bounded `.xlsx` worksheet reader (GX-004).
//!
//! [`parse_xlsx`] reads the first worksheet of an Office Open XML workbook
//! and turns it into a [`XlsxRowsAdapter`]: the first non-empty row supplies
//! the header names and every later non-empty row becomes one object. Cells
//! are carried as text, like CSV: shared and inline strings are resolved,
//! boolean cells become `TRUE`/`FALSE`, and a number keeps the exact text the
//! workbook stored. Dates and styles are not converted, so a date cell keeps
//! its serial number.
//!
//! The reader is deliberately narrow and bounded. The package may inflate to
//! at most [`MAX_XLSX_INFLATED_BYTES`], one part to at most
//! [`MAX_XLSX_PART_BYTES`], and it reads only the workbook part, its
//! relationship map, the optional shared-string table, and the selected
//! worksheet. Rows, columns, shared strings, and one cell's text have hard
//! caps, and a malformed package, a missing part, an unresolved shared
//! string, or a data cell under a blank header is refused rather than
//! guessed at.

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
use crate::xpath::{child_elements, element_attribute_map, element_tag, element_text};

/// Largest workbook package accepted, in bytes.
pub const MAX_XLSX_BYTES: usize = 32 * 1024 * 1024;
/// Largest one inflated workbook part accepted, in bytes.
pub const MAX_XLSX_PART_BYTES: u64 = 16 * 1024 * 1024;
/// Most bytes every read part may inflate to together.
pub const MAX_XLSX_INFLATED_BYTES: u64 = 64 * 1024 * 1024;
/// Most non-empty rows read, including the header row.
pub const MAX_XLSX_ROWS: usize = 50_000;
/// Most columns one worksheet may name.
pub const MAX_XLSX_COLUMNS: usize = 256;
/// Most shared strings one workbook may declare.
pub const MAX_XLSX_SHARED_STRINGS: usize = 200_000;
/// Longest text one cell may carry, in UTF-8 bytes.
pub const MAX_XLSX_CELL_BYTES: usize = 64 * 1024;
/// Longest worksheet name accepted, in UTF-8 bytes.
pub const MAX_XLSX_SHEET_NAME_BYTES: usize = 128;

/// Why a workbook or worksheet could not be read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XlsxError {
    /// The package is larger than [`MAX_XLSX_BYTES`].
    PackageTooLarge { actual: usize, maximum: usize },
    /// The bytes are not a readable ZIP package.
    NotPackage,
    /// A part the workbook needs is absent.
    MissingPart(String),
    /// A part inflates past the per-part or total bound.
    PartTooLarge(String),
    /// A part is not valid XML.
    Xml { part: String, error: ParseError },
    /// The workbook lists no worksheet.
    NoSheets,
    /// A requested worksheet name is absent.
    UnknownSheet(String),
    /// A sheet has no relationship target.
    MissingRelationship(String),
    /// A relationship target leaves the package's `xl` directory.
    InvalidTarget(String),
    /// A cell reference is not `A1`-style.
    InvalidCellReference(String),
    /// A shared-string index has no entry.
    UnknownSharedString(u32),
    /// A boolean cell is neither zero nor one.
    InvalidBoolean(String),
    /// A cell type is not supported.
    UnsupportedCellType(String),
    /// One cell's text is too long.
    CellTooLong { row: usize, column: usize },
    /// A worksheet names too many columns.
    TooManyColumns { maximum: usize },
    /// A worksheet has too many non-empty rows.
    TooManyRows { maximum: usize },
    /// A workbook has too many shared strings.
    TooManySharedStrings { maximum: usize },
    /// The first non-empty row has no named column.
    MissingHeaders,
    /// Two header cells carry the same name.
    DuplicateHeader(String),
    /// A header name is too long.
    HeaderTooLong { column: usize },
    /// A data cell sits under a blank header.
    UnlabelledColumn { row: usize, column: usize },
}

impl fmt::Display for XlsxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PackageTooLarge { actual, maximum } => {
                write!(formatter, "workbook is {actual} bytes; limit is {maximum}")
            }
            Self::NotPackage => formatter.write_str("workbook is not a ZIP package"),
            Self::MissingPart(part) => write!(formatter, "workbook has no {part}"),
            Self::PartTooLarge(part) => write!(formatter, "{part} inflates past its bound"),
            Self::Xml { part, error } => write!(formatter, "{part} is not valid XML: {error}"),
            Self::NoSheets => formatter.write_str("workbook lists no worksheet"),
            Self::UnknownSheet(name) => write!(formatter, "workbook has no worksheet named {name}"),
            Self::MissingRelationship(id) => {
                write!(formatter, "worksheet relationship {id} has no target")
            }
            Self::InvalidTarget(target) => {
                write!(formatter, "worksheet target {target} leaves the workbook")
            }
            Self::InvalidCellReference(reference) => {
                write!(formatter, "cell reference {reference} is not A1-style")
            }
            Self::UnknownSharedString(index) => {
                write!(formatter, "shared string {index} is not in the table")
            }
            Self::InvalidBoolean(value) => {
                write!(formatter, "boolean cell value {value} is not 0 or 1")
            }
            Self::UnsupportedCellType(kind) => {
                write!(formatter, "cell type {kind} is not supported")
            }
            Self::CellTooLong { row, column } => write!(
                formatter,
                "cell at row {row} column {column} is longer than {MAX_XLSX_CELL_BYTES} bytes"
            ),
            Self::TooManyColumns { maximum } => {
                write!(formatter, "worksheet names more than {maximum} columns")
            }
            Self::TooManyRows { maximum } => {
                write!(
                    formatter,
                    "worksheet has more than {maximum} non-empty rows"
                )
            }
            Self::TooManySharedStrings { maximum } => {
                write!(formatter, "workbook has more than {maximum} shared strings")
            }
            Self::MissingHeaders => formatter.write_str("worksheet has no named columns"),
            Self::DuplicateHeader(header) => write!(formatter, "duplicate header `{header}`"),
            Self::HeaderTooLong { column } => {
                write!(formatter, "header at column {column} is too long")
            }
            Self::UnlabelledColumn { row, column } => {
                write!(formatter, "cell at row {row} column {column} has no header")
            }
        }
    }
}

impl std::error::Error for XlsxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Xml { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// A parsed worksheet: named columns and text rows for [`DataAdapter`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XlsxRowsAdapter {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl XlsxRowsAdapter {
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

impl DataAdapter for XlsxRowsAdapter {
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

/// Reads the first worksheet of an Office Open XML workbook.
///
/// # Errors
///
/// Returns [`XlsxError`] for a non-ZIP package, a missing or oversized part, a
/// malformed worksheet, or a violated row, column, shared-string, or cell
/// bound.
pub fn parse_xlsx(bytes: &[u8]) -> Result<XlsxRowsAdapter, XlsxError> {
    parse_workbook(bytes, None)
}

/// Reads one named worksheet of an Office Open XML workbook.
///
/// # Errors
///
/// Returns [`XlsxError`] for a non-ZIP package, an unknown sheet, a missing or
/// oversized part, a malformed worksheet, or a violated row, column,
/// shared-string, or cell bound.
pub fn parse_xlsx_sheet(bytes: &[u8], sheet_name: &str) -> Result<XlsxRowsAdapter, XlsxError> {
    parse_workbook(bytes, Some(sheet_name))
}

fn parse_workbook(bytes: &[u8], selected: Option<&str>) -> Result<XlsxRowsAdapter, XlsxError> {
    if bytes.len() > MAX_XLSX_BYTES {
        return Err(XlsxError::PackageTooLarge {
            actual: bytes.len(),
            maximum: MAX_XLSX_BYTES,
        });
    }
    let mut package = Package::open(bytes)?;
    let workbook = parse_part(&mut package, "xl/workbook.xml", MAX_XLSX_PART_BYTES)?;
    let sheets = sheet_targets(&workbook, &mut package)?;
    let sheet = match selected {
        Some(name) => sheets
            .iter()
            .find(|sheet| sheet.name == name)
            .ok_or_else(|| XlsxError::UnknownSheet(name.to_owned()))?,
        None => sheets.first().ok_or(XlsxError::NoSheets)?,
    };
    let worksheet = parse_part(&mut package, &sheet.path, MAX_XLSX_PART_BYTES)?;
    let shared = match package.read("xl/sharedStrings.xml", MAX_XLSX_PART_BYTES)? {
        Some(bytes) => parse_shared_strings(&bytes)?,
        None => Vec::new(),
    };
    worksheet_rows(&worksheet, &shared)
}

struct Package<'a> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    remaining: u64,
}

impl<'a> Package<'a> {
    fn open(bytes: &'a [u8]) -> Result<Self, XlsxError> {
        ZipArchive::new(Cursor::new(bytes))
            .map(|archive| Self {
                archive,
                remaining: MAX_XLSX_INFLATED_BYTES,
            })
            .map_err(|_| XlsxError::NotPackage)
    }

    /// At most `limit` bytes of part `name`, charged to the package budget.
    fn read(&mut self, name: &str, limit: u64) -> Result<Option<Vec<u8>>, XlsxError> {
        let Ok(file) = self.archive.by_name(name) else {
            return Ok(None);
        };
        let allowed = limit.min(self.remaining);
        let mut bytes = Vec::new();
        file.take(allowed.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| XlsxError::MissingPart(name.to_owned()))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > allowed {
            return Err(XlsxError::PartTooLarge(name.to_owned()));
        }
        self.remaining = self.remaining.saturating_sub(bytes.len() as u64);
        Ok(Some(bytes))
    }
}

fn parse_part(package: &mut Package<'_>, name: &str, limit: u64) -> Result<DataValue, XlsxError> {
    let bytes = package
        .read(name, limit)?
        .ok_or_else(|| XlsxError::MissingPart(name.to_owned()))?;
    let limits = ParseLimits {
        max_input_bytes: usize::try_from(limit).unwrap_or(usize::MAX),
        max_rows: MAX_XLSX_ROWS,
        max_columns: MAX_XLSX_COLUMNS,
        max_field_bytes: MAX_XLSX_CELL_BYTES,
        max_depth: 32,
        max_nodes: 2_000_000,
    };
    parse_xml(&bytes, &limits).map_err(|error| XlsxError::Xml {
        part: name.to_owned(),
        error,
    })
}

struct SheetTarget {
    name: String,
    path: String,
}

fn sheet_targets(
    workbook: &DataValue,
    package: &mut Package<'_>,
) -> Result<Vec<SheetTarget>, XlsxError> {
    let sheets = child_named(workbook, "sheets").ok_or(XlsxError::MissingHeaders)?;
    let mut listed = Vec::new();
    for sheet in children_named(sheets, "sheet") {
        let name = attribute(sheet, "name").ok_or(XlsxError::MissingHeaders)?;
        if name.is_empty() || name.len() > MAX_XLSX_SHEET_NAME_BYTES {
            return Err(XlsxError::UnknownSheet(name.to_owned()));
        }
        let relationship = attribute(sheet, "r:id")
            .or_else(|| attribute_suffix(sheet, ":id"))
            .ok_or_else(|| XlsxError::MissingRelationship(name.to_owned()))?;
        listed.push((name.to_owned(), relationship.to_owned()));
    }
    if listed.is_empty() {
        return Err(XlsxError::NoSheets);
    }
    let rels = parse_part(package, "xl/_rels/workbook.xml.rels", MAX_XLSX_PART_BYTES)?;
    let mut targets = BTreeMap::new();
    for relationship in children_named(&rels, "Relationship") {
        let (Some(id), Some(target)) = (
            attribute(relationship, "Id"),
            attribute(relationship, "Target"),
        ) else {
            continue;
        };
        targets.insert(id.to_owned(), target.to_owned());
    }
    let mut sheets = Vec::with_capacity(listed.len());
    for (name, relationship) in listed {
        let target = targets
            .get(&relationship)
            .ok_or_else(|| XlsxError::MissingRelationship(relationship.clone()))?;
        sheets.push(SheetTarget {
            name,
            path: resolve_target(target)?,
        });
    }
    Ok(sheets)
}

fn resolve_target(target: &str) -> Result<String, XlsxError> {
    let target = target.replace('\\', "/");
    let target = target.strip_prefix('/').unwrap_or(&target);
    let path = if target.starts_with("xl/") {
        target.to_owned()
    } else {
        format!("xl/{target}")
    };
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(XlsxError::InvalidTarget(target.to_owned())),
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err(XlsxError::InvalidTarget(target.to_owned()));
    }
    Ok(parts.join("/"))
}

fn parse_shared_strings(bytes: &[u8]) -> Result<Vec<String>, XlsxError> {
    let limits = ParseLimits {
        max_input_bytes: usize::try_from(MAX_XLSX_PART_BYTES).unwrap_or(usize::MAX),
        max_rows: MAX_XLSX_ROWS,
        max_columns: MAX_XLSX_COLUMNS,
        max_field_bytes: MAX_XLSX_CELL_BYTES,
        max_depth: 32,
        max_nodes: 2_000_000,
    };
    let document = parse_xml(bytes, &limits).map_err(|error| XlsxError::Xml {
        part: "xl/sharedStrings.xml".to_owned(),
        error,
    })?;
    let mut strings = Vec::new();
    for item in children_named(&document, "si") {
        strings.push(string_item_text(item));
        if strings.len() > MAX_XLSX_SHARED_STRINGS {
            return Err(XlsxError::TooManySharedStrings {
                maximum: MAX_XLSX_SHARED_STRINGS,
            });
        }
    }
    Ok(strings)
}

/// Text of one shared-string item: direct `t` runs and rich-text `r` runs.
fn string_item_text(item: &DataValue) -> String {
    let mut text = String::new();
    for child in child_elements(item) {
        match element_tag(child) {
            Some("t") => text.push_str(&element_text(child)),
            Some("r") => {
                for run in children_named(child, "t") {
                    text.push_str(&element_text(run));
                }
            }
            _ => {}
        }
    }
    text
}

fn worksheet_rows(worksheet: &DataValue, shared: &[String]) -> Result<XlsxRowsAdapter, XlsxError> {
    let sheet_data = child_named(worksheet, "sheetData").ok_or(XlsxError::MissingHeaders)?;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row_number = 0usize;
    for row in children_named(sheet_data, "row") {
        row_number = match attribute(row, "r") {
            Some(value) => parse_row_number(value)?,
            None => row_number.saturating_add(1),
        };
        let mut cells: Vec<Option<String>> = Vec::new();
        let mut last_column: Option<usize> = None;
        for cell in children_named(row, "c") {
            let column = match attribute(cell, "r") {
                Some(reference) => column_index(reference)?,
                None => last_column.map_or(0, |column| column.saturating_add(1)),
            };
            if column >= MAX_XLSX_COLUMNS {
                return Err(XlsxError::TooManyColumns {
                    maximum: MAX_XLSX_COLUMNS,
                });
            }
            last_column = Some(column);
            let value = cell_value(cell, shared)?;
            if value.len() > MAX_XLSX_CELL_BYTES {
                return Err(XlsxError::CellTooLong {
                    row: row_number,
                    column: column + 1,
                });
            }
            if cells.len() <= column {
                cells.resize(column + 1, None);
            }
            cells[column] = Some(value);
        }
        let cells: Vec<String> = cells.into_iter().map(Option::unwrap_or_default).collect();
        if cells.iter().all(String::is_empty) {
            continue;
        }
        rows.push(cells);
        if rows.len() > MAX_XLSX_ROWS {
            return Err(XlsxError::TooManyRows {
                maximum: MAX_XLSX_ROWS,
            });
        }
    }
    let header = rows.first().ok_or(XlsxError::MissingHeaders)?;
    let mut columns = Vec::new();
    for (column, value) in header.iter().enumerate() {
        let name = value.trim();
        if name.is_empty() {
            continue;
        }
        if name.len() > MAX_XLSX_CELL_BYTES {
            return Err(XlsxError::HeaderTooLong { column: column + 1 });
        }
        if columns.iter().any(|(_, existing)| existing == name) {
            return Err(XlsxError::DuplicateHeader(name.to_owned()));
        }
        columns.push((column, name.to_owned()));
    }
    if columns.is_empty() {
        return Err(XlsxError::MissingHeaders);
    }
    let mut output = Vec::with_capacity(rows.len().saturating_sub(1));
    for (index, row) in rows.iter().enumerate().skip(1) {
        for (column, value) in row.iter().enumerate() {
            if !value.is_empty() && !columns.iter().any(|(named, _)| *named == column) {
                return Err(XlsxError::UnlabelledColumn {
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
    Ok(XlsxRowsAdapter {
        headers: columns.into_iter().map(|(_, name)| name).collect(),
        rows: output,
    })
}

fn cell_value(cell: &DataValue, shared: &[String]) -> Result<String, XlsxError> {
    let kind = attribute(cell, "t").unwrap_or("n");
    let raw = child_named(cell, "v").map(element_text).unwrap_or_default();
    match kind {
        "s" => {
            if raw.trim().is_empty() {
                return Ok(String::new());
            }
            let index = raw
                .trim()
                .parse::<u32>()
                .map_err(|_| XlsxError::UnknownSharedString(0))?;
            shared
                .get(usize::try_from(index).unwrap_or(usize::MAX))
                .cloned()
                .ok_or(XlsxError::UnknownSharedString(index))
        }
        "inlineStr" => Ok(child_named(cell, "is")
            .map(string_item_text)
            .unwrap_or_default()),
        "b" => match raw.trim() {
            "" => Ok(String::new()),
            "0" => Ok("FALSE".to_owned()),
            "1" => Ok("TRUE".to_owned()),
            value => Err(XlsxError::InvalidBoolean(value.to_owned())),
        },
        "str" | "n" | "d" | "e" => Ok(raw),
        other => Err(XlsxError::UnsupportedCellType(other.to_owned())),
    }
}

fn column_index(reference: &str) -> Result<usize, XlsxError> {
    let letters = reference
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .count();
    if letters == 0
        || letters != reference.len().min(letters)
        || !reference[letters..]
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        || reference[letters..].is_empty()
    {
        return Err(XlsxError::InvalidCellReference(reference.to_owned()));
    }
    let mut column = 0usize;
    for byte in reference[..letters].bytes() {
        column = column
            .checked_mul(26)
            .and_then(|value| {
                value.checked_add(usize::from(byte.to_ascii_uppercase().wrapping_sub(b'A')) + 1)
            })
            .ok_or_else(|| XlsxError::InvalidCellReference(reference.to_owned()))?;
    }
    column
        .checked_sub(1)
        .ok_or_else(|| XlsxError::InvalidCellReference(reference.to_owned()))
}

fn parse_row_number(value: &str) -> Result<usize, XlsxError> {
    value
        .parse::<usize>()
        .ok()
        .filter(|number| *number >= 1)
        .ok_or_else(|| XlsxError::InvalidCellReference(value.to_owned()))
}

fn child_named<'a>(node: &'a DataValue, name: &'a str) -> Option<&'a DataValue> {
    children_named(node, name).next()
}

fn children_named<'a>(
    node: &'a DataValue,
    name: &'a str,
) -> impl Iterator<Item = &'a DataValue> + 'a {
    child_elements(node).filter(move |child| element_tag(child) == Some(name))
}

fn attribute<'a>(node: &'a DataValue, name: &str) -> Option<&'a str> {
    match element_attribute_map(node)?.get(name)? {
        DataValue::String(value) => Some(value),
        _ => None,
    }
}

fn attribute_suffix<'a>(node: &'a DataValue, suffix: &str) -> Option<&'a str> {
    element_attribute_map(node)?
        .iter()
        .find(|(name, _)| name.ends_with(suffix))
        .and_then(|(_, value)| match value {
            DataValue::String(value) => Some(value.as_str()),
            _ => None,
        })
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

    const WORKBOOK: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
        <workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
                  xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
          <sheets>
            <sheet name="Data" sheetId="1" r:id="rId1"/>
            <sheet name="Other" sheetId="2" r:id="rId2"/>
          </sheets>
        </workbook>"#;

    const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
        <Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
          <Relationship Id="rId1" Type="worksheet" Target="worksheets/sheet1.xml"/>
          <Relationship Id="rId2" Type="worksheet" Target="/xl/worksheets/sheet2.xml"/>
        </Relationships>"#;

    const SHARED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
        <sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
          <si><t>Name</t></si>
          <si><t>Score</t></si>
          <si><t>Active</t></si>
          <si><t>Alice</t></si>
          <si><r><t>Bo</t></r><r><t>b</t></r></si>
        </sst>"#;

    const SHEET: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
        <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
          <sheetData>
            <row r="1">
              <c r="A1" t="s"><v>0</v></c>
              <c r="B1" t="s"><v>1</v></c>
              <c r="C1" t="s"><v>2</v></c>
            </row>
            <row r="2">
              <c r="A2" t="s"><v>3</v></c>
              <c r="B2"><v>42.5</v></c>
              <c r="C2" t="b"><v>1</v></c>
            </row>
            <row r="3">
              <c r="A3" t="inlineStr"><is><t>Inline</t></is></c>
              <c r="C3" t="str"><v>pending</v></c>
            </row>
            <row r="4"></row>
            <row r="5">
              <c r="A5" t="s"><v>4</v></c>
              <c r="B5"><v>-3</v></c>
              <c r="C5" t="b"><v>0</v></c>
            </row>
          </sheetData>
        </worksheet>"#;

    const OTHER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
        <worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
          <sheetData>
            <row><c><v>Label</v></c><c><v>Value</v></c></row>
            <row><c><v>one</v></c><c><v>1</v></c></row>
          </sheetData>
        </worksheet>"#;

    fn workbook() -> Vec<u8> {
        package(&[
            ("xl/workbook.xml", WORKBOOK),
            ("xl/_rels/workbook.xml.rels", RELS),
            ("xl/sharedStrings.xml", SHARED),
            ("xl/worksheets/sheet1.xml", SHEET),
            ("xl/worksheets/sheet2.xml", OTHER),
        ])
    }

    #[test]
    fn a_worksheet_becomes_header_keyed_text_rows() {
        let adapter = parse_xlsx(&workbook()).unwrap();
        assert_eq!(adapter.headers(), ["Name", "Score", "Active"]);
        assert_eq!(
            adapter.rows(),
            [
                vec!["Alice".to_owned(), "42.5".to_owned(), "TRUE".to_owned()],
                vec!["Inline".to_owned(), String::new(), "pending".to_owned()],
                vec!["Bob".to_owned(), "-3".to_owned(), "FALSE".to_owned()],
            ]
        );
        let DataValue::List(rows) = adapter.representation().unwrap() else {
            panic!("an XLSX adapter returns a list");
        };
        assert_eq!(rows.len(), 3);
        let DataValue::Object(first) = &rows[0] else {
            panic!("an XLSX row is an object");
        };
        assert_eq!(first["Name"], DataValue::from("Alice"));
        assert_eq!(first["Score"], DataValue::from("42.5"));
        assert_eq!(first["Active"], DataValue::from("TRUE"));
    }

    #[test]
    fn a_named_sheet_and_absolute_relationship_target_are_read() {
        let adapter = parse_xlsx_sheet(&workbook(), "Other").unwrap();
        assert_eq!(adapter.headers(), ["Label", "Value"]);
        assert_eq!(adapter.rows(), [vec!["one".to_owned(), "1".to_owned()]]);
    }

    #[test]
    fn malformed_or_ambiguous_workbooks_are_refused() {
        assert_eq!(parse_xlsx(b"not a zip"), Err(XlsxError::NotPackage));
        assert_eq!(
            parse_xlsx_sheet(&workbook(), "Missing"),
            Err(XlsxError::UnknownSheet("Missing".to_owned()))
        );

        let duplicate = package(&[
            ("xl/workbook.xml", WORKBOOK),
            ("xl/_rels/workbook.xml.rels", RELS),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row><c r="A1"><v>Name</v></c><c r="B1"><v>Name</v></c></row></sheetData></worksheet>"#,
            ),
        ]);
        assert_eq!(
            parse_xlsx(&duplicate),
            Err(XlsxError::DuplicateHeader("Name".to_owned()))
        );

        let unlabelled = package(&[
            ("xl/workbook.xml", WORKBOOK),
            ("xl/_rels/workbook.xml.rels", RELS),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row><c r="A1"><v>Name</v></c></row><row><c r="A2"><v>x</v></c><c r="B2"><v>y</v></c></row></sheetData></worksheet>"#,
            ),
        ]);
        assert_eq!(
            parse_xlsx(&unlabelled),
            Err(XlsxError::UnlabelledColumn { row: 2, column: 2 })
        );

        let bad_shared = package(&[
            ("xl/workbook.xml", WORKBOOK),
            ("xl/_rels/workbook.xml.rels", RELS),
            ("xl/sharedStrings.xml", SHARED),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row><c r="A1" t="s"><v>99</v></c></row></sheetData></worksheet>"#,
            ),
        ]);
        assert_eq!(
            parse_xlsx(&bad_shared),
            Err(XlsxError::UnknownSharedString(99))
        );
    }
}
