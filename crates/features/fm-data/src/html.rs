//! A bounded HTML table reader (GX-004).
//!
//! The reader takes the first `<table>` in a document and turns its rows
//! into typed objects: the first row names the columns and each later row is
//! one [`DataValue::Object`] keyed by those names. Cell text has its tags
//! stripped and XML entities decoded; whitespace runs collapse to one space.
//! Cells may use `<td>` or `<th>`. Every size, row, column, and field bound
//! comes from [`ParseLimits`], and unterminated markup is refused rather
//! than guessed at.

use crate::parse::{ParseError, ParseErrorKind, ParseLimits, decode};
use crate::value::DataValue;
use crate::xml::decode_entities;
use std::collections::BTreeSet;

/// Reads the first table of an HTML document as rows of named cells.
///
/// # Errors
///
/// Returns a located [`ParseError`] for malformed markup, duplicate or empty
/// headers, ragged rows, or limit violations.
#[allow(clippy::too_many_lines)] // one branch per table section
pub fn parse_html_table(bytes: &[u8], limits: &ParseLimits) -> Result<Vec<DataValue>, ParseError> {
    let text = decode(bytes, limits)?;
    let lower = text.to_ascii_lowercase();
    let start = lower.find("<table").ok_or_else(|| {
        ParseError::at(
            text,
            0,
            ParseErrorKind::InvalidXml {
                reason: "no table element",
            },
        )
    })?;
    let end = lower[start..]
        .find("</table>")
        .map_or(lower.len(), |end| start + end);
    let mut rows = Vec::new();
    let mut position = start;
    while let Some(found) = lower[position..end].find("<tr") {
        let row_start = position + found;
        let row_end = lower[row_start..end]
            .find("</tr>")
            .map_or(end, |end| row_start + end);
        let mut cells = Vec::new();
        let mut cell_position = row_start;
        while let Some(found) = cell_tag(&lower, cell_position, row_end) {
            let (tag_start, tag_end) = found;
            let closing = if lower[tag_start..].starts_with("<th") {
                "</th>"
            } else {
                "</td>"
            };
            let Some(close) = lower[tag_end..row_end].find(closing) else {
                return Err(ParseError::at(
                    text,
                    tag_end,
                    ParseErrorKind::InvalidXml {
                        reason: "unterminated table cell",
                    },
                ));
            };
            let close = tag_end + close;
            cells.push(normalize(&text[tag_end..close], tag_end, text)?);
            if cells.len() > limits.max_columns {
                return Err(ParseError::at(
                    text,
                    tag_start,
                    ParseErrorKind::TooManyColumns {
                        limit: limits.max_columns,
                    },
                ));
            }
            cell_position = close + closing.len();
        }
        if !cells.is_empty() {
            rows.push(cells);
            if rows.len() > limits.max_rows.saturating_add(1) {
                return Err(ParseError::at(
                    text,
                    row_start,
                    ParseErrorKind::TooManyRows {
                        limit: limits.max_rows,
                    },
                ));
            }
        }
        position = row_end + 5;
    }
    let Some((headers, data)) = rows.split_first() else {
        return Ok(Vec::new());
    };
    let mut seen = BTreeSet::new();
    for (column, header) in headers.iter().enumerate() {
        if header.is_empty() {
            return Err(ParseError::at(
                text,
                start,
                ParseErrorKind::EmptyHeader { column },
            ));
        }
        if !seen.insert(header.as_str()) {
            return Err(ParseError::at(
                text,
                start,
                ParseErrorKind::DuplicateHeader(header.clone()),
            ));
        }
    }
    for (row, cells) in data.iter().enumerate() {
        if cells.len() != headers.len() {
            return Err(ParseError::at(
                text,
                start,
                ParseErrorKind::RowWidth {
                    row,
                    expected: headers.len(),
                    actual: cells.len(),
                },
            ));
        }
    }
    Ok(data
        .iter()
        .map(|cells| {
            DataValue::Object(
                headers
                    .iter()
                    .cloned()
                    .zip(cells.iter().cloned().map(DataValue::String))
                    .collect(),
            )
        })
        .collect())
}

/// The next `<td`/`<th` opening tag in `start..end`, as `(start, after '>')`.
fn cell_tag(lower: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let td = lower[start..end].find("<td").map(|found| start + found);
    let th = lower[start..end].find("<th").map(|found| start + found);
    let tag_start = match (td, th) {
        (Some(td), Some(th)) => td.min(th),
        (Some(td), None) => td,
        (None, Some(th)) => th,
        (None, None) => return None,
    };
    let tag_end = lower[tag_start..end].find('>')? + tag_start + 1;
    Some((tag_start, tag_end))
}

/// Strips tags, decodes entities, and collapses whitespace.
fn normalize(raw: &str, offset: usize, text: &str) -> Result<String, ParseError> {
    let mut stripped = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(open) = rest.find('<') {
        stripped.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            return Err(ParseError::at(
                text,
                offset + raw.len() - rest.len() + open,
                ParseErrorKind::InvalidXml {
                    reason: "unterminated tag in table cell",
                },
            ));
        };
        rest = &rest[open + close + 1..];
    }
    stripped.push_str(rest);
    let decoded = decode_entities(&stripped, offset, text)?;
    Ok(decoded.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: ParseLimits = ParseLimits {
        max_input_bytes: 64 * 1024,
        max_rows: 10,
        max_columns: 4,
        max_field_bytes: 1024,
        max_depth: 8,
        max_nodes: 1000,
    };

    fn field<'a>(row: &'a DataValue, name: &str) -> Option<&'a str> {
        let DataValue::Object(object) = row else {
            return None;
        };
        match object.get(name)? {
            DataValue::String(value) => Some(value),
            _ => None,
        }
    }

    #[test]
    fn the_first_row_names_the_columns() {
        let rows = parse_html_table(
            br#"<html><body>
                  <table class="data">
                    <tr><th>Name</th><th>Score</th></tr>
                    <tr><td>Ada</td><td><b>7</b> &amp; rising</td></tr>
                    <tr><td>Grace</td><td>9</td></tr>
                  </table>
                </body></html>"#,
            &LIMITS,
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(field(&rows[0], "Name"), Some("Ada"));
        assert_eq!(field(&rows[0], "Score"), Some("7 & rising"));
        assert_eq!(field(&rows[1], "Name"), Some("Grace"));
    }

    #[test]
    fn ragged_rows_and_duplicate_headers_are_refused() {
        assert!(
            parse_html_table(
                b"<table><tr><th>A</th></tr><tr><td>1</td><td>2</td></tr></table>",
                &LIMITS
            )
            .is_err()
        );
        assert!(
            parse_html_table(
                b"<table><tr><th>A</th><th>A</th></tr><tr><td>1</td><td>2</td></tr></table>",
                &LIMITS
            )
            .is_err()
        );
        assert!(parse_html_table(b"<p>no table</p>", &LIMITS).is_err());
    }

    #[test]
    fn a_table_without_data_rows_is_empty() {
        assert_eq!(
            parse_html_table(b"<table></table>", &LIMITS).unwrap(),
            Vec::new()
        );
    }
}
