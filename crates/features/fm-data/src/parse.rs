//! Bounded, deterministic parsers for in-memory CSV and JSON bytes.
//!
//! Parsers never perform I/O. Every allocation is bounded by [`ParseLimits`],
//! and every failure carries a byte offset plus a 1-based line and column.

use std::{collections::BTreeMap, fmt};

use crate::{CsvRowsAdapter, DataValue, Decimal};

/// Explicit resource limits shared by the CSV and JSON parsers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseLimits {
    /// Maximum accepted input size in bytes.
    pub max_input_bytes: usize,
    /// Maximum CSV data rows (excluding the header).
    pub max_rows: usize,
    /// Maximum CSV columns per record.
    pub max_columns: usize,
    /// Maximum decoded bytes in one CSV field, JSON string, or JSON key.
    pub max_field_bytes: usize,
    /// Maximum JSON array/object nesting depth.
    pub max_depth: usize,
    /// Maximum JSON values (scalars and containers) in one document.
    pub max_nodes: usize,
}

impl ParseLimits {
    pub const DEFAULT_MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
    pub const DEFAULT_MAX_ROWS: usize = 10_000;
    pub const DEFAULT_MAX_COLUMNS: usize = 256;
    pub const DEFAULT_MAX_FIELD_BYTES: usize = 64 * 1024;
    pub const DEFAULT_MAX_DEPTH: usize = 32;
    pub const DEFAULT_MAX_NODES: usize = 100_000;
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: Self::DEFAULT_MAX_INPUT_BYTES,
            max_rows: Self::DEFAULT_MAX_ROWS,
            max_columns: Self::DEFAULT_MAX_COLUMNS,
            max_field_bytes: Self::DEFAULT_MAX_FIELD_BYTES,
            max_depth: Self::DEFAULT_MAX_DEPTH,
            max_nodes: Self::DEFAULT_MAX_NODES,
        }
    }
}

/// The reason a parse failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseErrorKind {
    InputTooLarge {
        limit: usize,
        actual: usize,
    },
    InvalidUtf8,
    InvalidDelimiter(u8),
    TooManyRows {
        limit: usize,
    },
    TooManyColumns {
        limit: usize,
    },
    FieldTooLarge {
        limit: usize,
    },
    DepthExceeded {
        limit: usize,
    },
    TooManyNodes {
        limit: usize,
    },
    /// A CSV quoted field or JSON string reached end of input.
    UnterminatedString,
    /// A `"` appeared inside an unquoted CSV field.
    UnexpectedQuote,
    /// Text followed the closing quote of a quoted CSV field.
    TextAfterQuote,
    EmptyHeader {
        column: usize,
    },
    DuplicateHeader(String),
    RowWidth {
        row: usize,
        expected: usize,
        actual: usize,
    },
    UnexpectedEnd,
    UnexpectedCharacter(char),
    InvalidEscape,
    ControlCharacterInString,
    InvalidNumber,
    NumberOutOfRange,
    DuplicateKey(String),
    TrailingData,
    /// XML parsing: the document must not carry a DOCTYPE.
    UnsupportedXmlDoctype,
    /// XML parsing: the document is malformed for this reason.
    InvalidXml {
        reason: &'static str,
    },
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge { limit, actual } => {
                write!(formatter, "input is {actual} bytes; limit is {limit}")
            }
            Self::InvalidUtf8 => formatter.write_str("input is not valid UTF-8"),
            Self::InvalidDelimiter(byte) => write!(formatter, "invalid CSV delimiter 0x{byte:02x}"),
            Self::TooManyRows { limit } => write!(formatter, "more than {limit} rows"),
            Self::TooManyColumns { limit } => write!(formatter, "more than {limit} columns"),
            Self::FieldTooLarge { limit } => write!(formatter, "field exceeds {limit} bytes"),
            Self::DepthExceeded { limit } => write!(formatter, "nesting exceeds depth {limit}"),
            Self::TooManyNodes { limit } => write!(formatter, "more than {limit} values"),
            Self::UnterminatedString => formatter.write_str("unterminated quoted string"),
            Self::UnexpectedQuote => formatter.write_str("quote inside unquoted CSV field"),
            Self::TextAfterQuote => formatter.write_str("text after closing quote"),
            Self::EmptyHeader { column } => write!(formatter, "CSV header {column} is empty"),
            Self::DuplicateHeader(header) => write!(formatter, "duplicate CSV header `{header}`"),
            Self::RowWidth {
                row,
                expected,
                actual,
            } => write!(
                formatter,
                "CSV row {row} has {actual} columns; expected {expected}"
            ),
            Self::UnexpectedEnd => formatter.write_str("unexpected end of input"),
            Self::UnexpectedCharacter(character) => {
                write!(formatter, "unexpected character {character:?}")
            }
            Self::InvalidEscape => formatter.write_str("invalid string escape"),
            Self::ControlCharacterInString => {
                formatter.write_str("unescaped control character in string")
            }
            Self::InvalidNumber => formatter.write_str("invalid number"),
            Self::NumberOutOfRange => {
                formatter.write_str("number is not representable as an exact integer or decimal")
            }
            Self::DuplicateKey(key) => write!(formatter, "duplicate object key `{key}`"),
            Self::TrailingData => formatter.write_str("trailing data after document"),
            Self::UnsupportedXmlDoctype => {
                formatter.write_str("XML documents must not carry a DOCTYPE")
            }
            Self::InvalidXml { reason } => write!(formatter, "invalid XML: {reason}"),
        }
    }
}

/// A located parse failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    /// Byte offset into the original input.
    pub offset: usize,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column, counted in Unicode scalar values.
    pub column: usize,
}

impl ParseError {
    pub(crate) fn at(text: &str, offset: usize, kind: ParseErrorKind) -> Self {
        let mut offset = offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        let prefix = &text[..offset];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        let column = prefix[line_start..].chars().count() + 1;
        Self {
            kind,
            offset,
            line,
            column,
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at line {}, column {}",
            self.kind, self.line, self.column
        )
    }
}

impl std::error::Error for ParseError {}

pub(crate) fn decode<'a>(bytes: &'a [u8], limits: &ParseLimits) -> Result<&'a str, ParseError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(ParseError {
            kind: ParseErrorKind::InputTooLarge {
                limit: limits.max_input_bytes,
                actual: bytes.len(),
            },
            offset: 0,
            line: 1,
            column: 1,
        });
    }
    std::str::from_utf8(bytes).map_err(|error| {
        let valid = error.valid_up_to();
        // The prefix is valid by definition, so this cannot fail.
        let prefix = std::str::from_utf8(&bytes[..valid]).unwrap_or_default();
        ParseError::at(prefix, valid, ParseErrorKind::InvalidUtf8)
    })
}

/// CSV dialect and limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CsvOptions {
    /// Single ASCII field delimiter; must not be `"`, CR, or LF.
    pub delimiter: u8,
    /// When `false`, columns are named `column_1`, `column_2`, ...
    pub has_header: bool,
    pub limits: ParseLimits,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            has_header: true,
            limits: ParseLimits::default(),
        }
    }
}

/// Parses RFC 4180-style CSV bytes into a header-checked [`CsvRowsAdapter`].
///
/// Accepted input: optional UTF-8 BOM, LF or CRLF (or bare CR) record
/// terminators, quoted fields with `""` escapes and embedded newlines.
/// Completely empty lines are skipped. Every data row must have exactly as
/// many fields as the header (or the first row when `has_header` is false).
///
/// # Errors
///
/// Returns a located [`ParseError`] for limit violations, malformed quoting,
/// empty or duplicate headers, and ragged rows.
pub fn parse_csv(bytes: &[u8], options: &CsvOptions) -> Result<CsvRowsAdapter, ParseError> {
    let limits = &options.limits;
    let text = decode(bytes, limits)?;
    let delimiter = options.delimiter;
    if !delimiter.is_ascii() || matches!(delimiter, b'"' | b'\r' | b'\n') {
        return Err(ParseError::at(
            text,
            0,
            ParseErrorKind::InvalidDelimiter(delimiter),
        ));
    }
    let start = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let mut reader = CsvReader {
        text,
        bytes: text.as_bytes(),
        pos: start,
        delimiter,
        limits,
    };
    let mut headers: Option<Vec<String>> = None;
    let mut rows = Vec::new();
    while let Some((record_offset, record)) = reader.next_record()? {
        if headers.is_none() {
            if options.has_header {
                headers = Some(validate_headers(text, record_offset, record)?);
                continue;
            }
            headers = Some(
                (1..=record.len())
                    .map(|index| format!("column_{index}"))
                    .collect(),
            );
        }
        let expected = headers.as_ref().map_or(0, Vec::len);
        if record.len() != expected {
            return Err(ParseError::at(
                text,
                record_offset,
                ParseErrorKind::RowWidth {
                    row: rows.len(),
                    expected,
                    actual: record.len(),
                },
            ));
        }
        if rows.len() == limits.max_rows {
            return Err(ParseError::at(
                text,
                record_offset,
                ParseErrorKind::TooManyRows {
                    limit: limits.max_rows,
                },
            ));
        }
        rows.push(record);
    }
    Ok(CsvRowsAdapter::new(headers.unwrap_or_default(), rows))
}

fn validate_headers(
    text: &str,
    offset: usize,
    record: Vec<String>,
) -> Result<Vec<String>, ParseError> {
    let mut seen = std::collections::BTreeSet::new();
    for (index, header) in record.iter().enumerate() {
        if header.is_empty() {
            return Err(ParseError::at(
                text,
                offset,
                ParseErrorKind::EmptyHeader { column: index },
            ));
        }
        if !seen.insert(header.as_str()) {
            return Err(ParseError::at(
                text,
                offset,
                ParseErrorKind::DuplicateHeader(header.clone()),
            ));
        }
    }
    Ok(record)
}

struct CsvReader<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    delimiter: u8,
    limits: &'a ParseLimits,
}

impl CsvReader<'_> {
    fn error(&self, offset: usize, kind: ParseErrorKind) -> ParseError {
        ParseError::at(self.text, offset, kind)
    }

    fn next_record(&mut self) -> Result<Option<(usize, Vec<String>)>, ParseError> {
        // Skip completely empty lines.
        loop {
            match self.bytes.get(self.pos) {
                None => return Ok(None),
                Some(b'\n') => self.pos += 1,
                Some(b'\r') => {
                    self.pos += 1;
                    if self.bytes.get(self.pos) == Some(&b'\n') {
                        self.pos += 1;
                    }
                }
                Some(_) => break,
            }
        }
        let record_offset = self.pos;
        let mut record = Vec::new();
        loop {
            let field_offset = self.pos;
            let field = if self.bytes.get(self.pos) == Some(&b'"') {
                self.quoted_field()?
            } else {
                self.unquoted_field()?
            };
            if field.len() > self.limits.max_field_bytes {
                return Err(self.error(
                    field_offset,
                    ParseErrorKind::FieldTooLarge {
                        limit: self.limits.max_field_bytes,
                    },
                ));
            }
            if record.len() == self.limits.max_columns {
                return Err(self.error(
                    field_offset,
                    ParseErrorKind::TooManyColumns {
                        limit: self.limits.max_columns,
                    },
                ));
            }
            record.push(field);
            match self.bytes.get(self.pos) {
                None => break,
                Some(byte) if *byte == self.delimiter => self.pos += 1,
                Some(b'\n') => {
                    self.pos += 1;
                    break;
                }
                Some(b'\r') => {
                    self.pos += 1;
                    if self.bytes.get(self.pos) == Some(&b'\n') {
                        self.pos += 1;
                    }
                    break;
                }
                Some(_) => return Err(self.error(self.pos, ParseErrorKind::TextAfterQuote)),
            }
        }
        Ok(Some((record_offset, record)))
    }

    fn unquoted_field(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        while let Some(&byte) = self.bytes.get(self.pos) {
            if byte == self.delimiter || byte == b'\n' || byte == b'\r' {
                break;
            }
            if byte == b'"' {
                return Err(self.error(self.pos, ParseErrorKind::UnexpectedQuote));
            }
            self.pos += 1;
        }
        Ok(self.text[start..self.pos].to_owned())
    }

    fn quoted_field(&mut self) -> Result<String, ParseError> {
        let open = self.pos;
        self.pos += 1;
        let mut field = String::new();
        loop {
            let run_start = self.pos;
            while self.bytes.get(self.pos).is_some_and(|byte| *byte != b'"') {
                self.pos += 1;
            }
            field.push_str(&self.text[run_start..self.pos]);
            if field.len() > self.limits.max_field_bytes {
                return Err(self.error(
                    open,
                    ParseErrorKind::FieldTooLarge {
                        limit: self.limits.max_field_bytes,
                    },
                ));
            }
            if self.pos >= self.bytes.len() {
                return Err(self.error(open, ParseErrorKind::UnterminatedString));
            }
            // At a quote: either an escaped quote or the closing quote.
            if self.bytes.get(self.pos + 1) == Some(&b'"') {
                field.push('"');
                self.pos += 2;
            } else {
                self.pos += 1;
                return Ok(field);
            }
        }
    }
}

impl CsvRowsAdapter {
    /// Parses CSV bytes. See [`parse_csv`].
    ///
    /// # Errors
    ///
    /// Returns a located [`ParseError`].
    pub fn parse(bytes: &[u8], options: &CsvOptions) -> Result<Self, ParseError> {
        parse_csv(bytes, options)
    }
}

/// Parses one JSON document into an exact [`DataValue`].
///
/// Numbers without a fraction or exponent become [`DataValue::Integer`];
/// all other numbers become exact [`DataValue::Decimal`] values or fail with
/// [`ParseErrorKind::NumberOutOfRange`]. Floating point is never used.
/// Duplicate object keys are rejected rather than silently overwritten.
///
/// # Errors
///
/// Returns a located [`ParseError`] for malformed JSON or limit violations.
pub fn parse_json(bytes: &[u8], limits: &ParseLimits) -> Result<DataValue, ParseError> {
    let text = decode(bytes, limits)?;
    let start = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let mut parser = JsonParser {
        text,
        bytes: text.as_bytes(),
        pos: start,
        depth: 0,
        nodes: 0,
        limits,
    };
    parser.skip_whitespace();
    let value = parser.value()?;
    parser.skip_whitespace();
    if parser.pos != parser.bytes.len() {
        return Err(parser.error(ParseErrorKind::TrailingData));
    }
    Ok(value)
}

struct JsonParser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
    nodes: usize,
    limits: &'a ParseLimits,
}

impl JsonParser<'_> {
    fn error(&self, kind: ParseErrorKind) -> ParseError {
        ParseError::at(self.text, self.pos, kind)
    }

    fn error_at(&self, offset: usize, kind: ParseErrorKind) -> ParseError {
        ParseError::at(self.text, offset, kind)
    }

    fn unexpected(&self) -> ParseError {
        match self.text[self.pos..].chars().next() {
            Some(character) => self.error(ParseErrorKind::UnexpectedCharacter(character)),
            None => self.error(ParseErrorKind::UnexpectedEnd),
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), ParseError> {
        if self.bytes.get(self.pos) == Some(&byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    fn count_node(&mut self) -> Result<(), ParseError> {
        if self.nodes == self.limits.max_nodes {
            return Err(self.error(ParseErrorKind::TooManyNodes {
                limit: self.limits.max_nodes,
            }));
        }
        self.nodes += 1;
        Ok(())
    }

    fn value(&mut self) -> Result<DataValue, ParseError> {
        self.count_node()?;
        match self.bytes.get(self.pos) {
            None => Err(self.error(ParseErrorKind::UnexpectedEnd)),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(DataValue::String),
            Some(b't') => self.literal("true", DataValue::Bool(true)),
            Some(b'f') => self.literal("false", DataValue::Bool(false)),
            Some(b'n') => self.literal("null", DataValue::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.unexpected()),
        }
    }

    fn literal(&mut self, word: &str, value: DataValue) -> Result<DataValue, ParseError> {
        if self.text[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.unexpected())
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        if self.depth == self.limits.max_depth {
            return Err(self.error(ParseErrorKind::DepthExceeded {
                limit: self.limits.max_depth,
            }));
        }
        self.depth += 1;
        self.pos += 1;
        Ok(())
    }

    fn array(&mut self) -> Result<DataValue, ParseError> {
        self.enter()?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.bytes.get(self.pos) == Some(&b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(DataValue::List(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value()?);
            self.skip_whitespace();
            match self.bytes.get(self.pos) {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(DataValue::List(items));
                }
                _ => return Err(self.unexpected()),
            }
        }
    }

    fn object(&mut self) -> Result<DataValue, ParseError> {
        self.enter()?;
        let mut fields = BTreeMap::new();
        self.skip_whitespace();
        if self.bytes.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(DataValue::Object(fields));
        }
        loop {
            self.skip_whitespace();
            let key_offset = self.pos;
            if self.bytes.get(self.pos) != Some(&b'"') {
                return Err(self.unexpected());
            }
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            let value = self.value()?;
            if fields.contains_key(&key) {
                return Err(self.error_at(key_offset, ParseErrorKind::DuplicateKey(key)));
            }
            fields.insert(key, value);
            self.skip_whitespace();
            match self.bytes.get(self.pos) {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(DataValue::Object(fields));
                }
                _ => return Err(self.unexpected()),
            }
        }
    }

    fn string(&mut self) -> Result<String, ParseError> {
        let open = self.pos;
        self.pos += 1;
        let mut output = String::new();
        loop {
            let run_start = self.pos;
            while let Some(&byte) = self.bytes.get(self.pos) {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            output.push_str(&self.text[run_start..self.pos]);
            if output.len() > self.limits.max_field_bytes {
                return Err(self.error_at(
                    open,
                    ParseErrorKind::FieldTooLarge {
                        limit: self.limits.max_field_bytes,
                    },
                ));
            }
            match self.bytes.get(self.pos) {
                None => return Err(self.error_at(open, ParseErrorKind::UnterminatedString)),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(output);
                }
                Some(b'\\') => {
                    let character = self.escape()?;
                    output.push(character);
                }
                Some(_) => return Err(self.error(ParseErrorKind::ControlCharacterInString)),
            }
        }
    }

    fn escape(&mut self) -> Result<char, ParseError> {
        let escape_offset = self.pos;
        self.pos += 1;
        let Some(&byte) = self.bytes.get(self.pos) else {
            return Err(self.error_at(escape_offset, ParseErrorKind::UnterminatedString));
        };
        self.pos += 1;
        let character = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{0008}',
            b'f' => '\u{000c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let high = self.hex4(escape_offset)?;
                let code = if (0xD800..0xDC00).contains(&high) {
                    if self.bytes.get(self.pos) != Some(&b'\\')
                        || self.bytes.get(self.pos + 1) != Some(&b'u')
                    {
                        return Err(self.error_at(escape_offset, ParseErrorKind::InvalidEscape));
                    }
                    self.pos += 2;
                    let low = self.hex4(escape_offset)?;
                    if !(0xDC00..0xE000).contains(&low) {
                        return Err(self.error_at(escape_offset, ParseErrorKind::InvalidEscape));
                    }
                    0x1_0000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                } else {
                    high
                };
                char::from_u32(code)
                    .ok_or_else(|| self.error_at(escape_offset, ParseErrorKind::InvalidEscape))?
            }
            _ => return Err(self.error_at(escape_offset, ParseErrorKind::InvalidEscape)),
        };
        Ok(character)
    }

    fn hex4(&mut self, escape_offset: usize) -> Result<u32, ParseError> {
        let digits = self
            .text
            .get(self.pos..self.pos + 4)
            .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| self.error_at(escape_offset, ParseErrorKind::InvalidEscape))?;
        let value = u32::from_str_radix(digits, 16)
            .map_err(|_| self.error_at(escape_offset, ParseErrorKind::InvalidEscape))?;
        self.pos += 4;
        Ok(value)
    }

    fn digits(&mut self) -> usize {
        let start = self.pos;
        while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
        }
        self.pos - start
    }

    fn number(&mut self) -> Result<DataValue, ParseError> {
        let start = self.pos;
        let negative = self.bytes.get(self.pos) == Some(&b'-');
        if negative {
            self.pos += 1;
        }
        let whole_start = self.pos;
        let whole_len = self.digits();
        if whole_len == 0 {
            return Err(self.error_at(start, ParseErrorKind::InvalidNumber));
        }
        if whole_len > 1 && self.bytes[whole_start] == b'0' {
            return Err(self.error_at(start, ParseErrorKind::InvalidNumber));
        }
        let whole = &self.text[whole_start..self.pos];
        let mut fraction = "";
        let mut exact_integer = true;
        if self.bytes.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            let fraction_start = self.pos;
            if self.digits() == 0 {
                return Err(self.error_at(start, ParseErrorKind::InvalidNumber));
            }
            fraction = &self.text[fraction_start..self.pos];
            exact_integer = false;
        }
        let mut exponent: i64 = 0;
        if matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            let exponent_negative = match self.bytes.get(self.pos) {
                Some(b'-') => {
                    self.pos += 1;
                    true
                }
                Some(b'+') => {
                    self.pos += 1;
                    false
                }
                _ => false,
            };
            let exponent_start = self.pos;
            let exponent_len = self.digits();
            if exponent_len == 0 {
                return Err(self.error_at(start, ParseErrorKind::InvalidNumber));
            }
            if exponent_len > 4 {
                return Err(self.error_at(start, ParseErrorKind::NumberOutOfRange));
            }
            let magnitude: i64 = self.text[exponent_start..self.pos]
                .parse()
                .map_err(|_| self.error_at(start, ParseErrorKind::InvalidNumber))?;
            exponent = if exponent_negative {
                -magnitude
            } else {
                magnitude
            };
            exact_integer = false;
        }
        if exact_integer {
            return self.text[start..self.pos]
                .parse::<i64>()
                .map(DataValue::Integer)
                .map_err(|_| self.error_at(start, ParseErrorKind::NumberOutOfRange));
        }
        exact_decimal(negative, whole, fraction, exponent)
            .map(DataValue::Decimal)
            .ok_or_else(|| self.error_at(start, ParseErrorKind::NumberOutOfRange))
    }
}

fn exact_decimal(negative: bool, whole: &str, fraction: &str, exponent: i64) -> Option<Decimal> {
    let mut digits: String = format!("{whole}{fraction}");
    let mut scale = i64::try_from(fraction.len()).ok()? - exponent;
    while scale > 0 && digits.ends_with('0') {
        digits.pop();
        scale -= 1;
    }
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return Some(Decimal::from_integer(0));
    }
    let mut significant = significant.to_owned();
    if scale < 0 {
        let zeros = usize::try_from(-scale).ok()?;
        if significant.len() + zeros > 19 {
            return None;
        }
        significant.extend(std::iter::repeat_n('0', zeros));
        scale = 0;
    }
    if significant.len() > 19 {
        return None;
    }
    let magnitude: u64 = significant.parse().ok()?;
    let coefficient = if negative {
        if magnitude == i64::MAX as u64 + 1 {
            i64::MIN
        } else {
            -i64::try_from(magnitude).ok()?
        }
    } else {
        i64::try_from(magnitude).ok()?
    };
    Decimal::new(coefficient, u32::try_from(scale).ok()?).ok()
}
