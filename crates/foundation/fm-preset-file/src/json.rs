//! The strict JSON subset Freemix documents use: integers only (no
//! fractions or exponents), no duplicate keys, no trailing commas, at most
//! [`MAX_JSON_DEPTH`] levels, plus typed field access that refuses unknown
//! and missing keys.

use std::{collections::BTreeMap, fmt::Write as _};

/// Deepest nesting a document may use.
pub const MAX_JSON_DEPTH: usize = 64;

/// A document that does not parse, or a value of the wrong shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxError {
    /// Byte offset of the problem, or `0` for a shape error.
    pub offset: usize,
    pub message: String,
}

/// A shape error (at offset `0`).
#[must_use]
pub fn syntax(message: impl Into<String>) -> SyntaxError {
    SyntaxError {
        offset: 0,
        message: message.into(),
    }
}

/// An enum field holding a value outside its names.
#[must_use]
pub fn unknown_enum(field: &str, value: &str) -> SyntaxError {
    syntax(format!("unknown {field} value `{value}`"))
}

/// One parsed JSON value.
#[derive(Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Number(u128),
    /// A negative integer, by magnitude.
    NegativeNumber(u128),
    String(String),
    Array(Vec<Self>),
    Object(BTreeMap<String, Self>),
}

/// A strict JSON reader over one document.
pub struct Reader<'a> {
    source: &'a str,
    offset: usize,
}

impl<'a> Reader<'a> {
    #[must_use]
    pub const fn new(source: &'a str) -> Self {
        Self { source, offset: 0 }
    }

    /// Parses the whole document.
    ///
    /// # Errors
    ///
    /// [`SyntaxError`] at the first byte outside the strict subset, or for
    /// trailing content.
    pub fn document(mut self) -> Result<Value, SyntaxError> {
        self.whitespace();
        let value = self.value(0)?;
        self.whitespace();
        if self.offset != self.source.len() {
            return Err(self.syntax("trailing content"));
        }
        Ok(value)
    }

    fn value(&mut self, depth: usize) -> Result<Value, SyntaxError> {
        if depth > MAX_JSON_DEPTH {
            return Err(self.syntax("JSON nesting limit exceeded"));
        }
        match self.source.as_bytes().get(self.offset) {
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => self.string().map(Value::String),
            Some(b't') if self.consume_literal("true") => Ok(Value::Bool(true)),
            Some(b'f') if self.consume_literal("false") => Ok(Value::Bool(false)),
            Some(b'n') if self.consume_literal("null") => Ok(Value::Null),
            Some(byte) if byte.is_ascii_digit() => self.number().map(Value::Number),
            Some(b'-') => {
                self.offset += 1;
                self.number().map(Value::NegativeNumber)
            }
            _ => Err(self.syntax("expected JSON value")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, SyntaxError> {
        self.expect_byte(b'{')?;
        let mut values = BTreeMap::new();
        self.whitespace();
        if self.consume_byte(b'}') {
            return Ok(Value::Object(values));
        }
        loop {
            let key = self.string()?;
            self.whitespace();
            self.expect_byte(b':')?;
            self.whitespace();
            let value = self.value(depth)?;
            if values.insert(key.clone(), value).is_some() {
                return Err(self.syntax(format!("duplicate field `{key}`")));
            }
            self.whitespace();
            if self.consume_byte(b'}') {
                break;
            }
            self.expect_byte(b',')?;
            self.whitespace();
            if self.next_is(b'}') {
                return Err(self.syntax("trailing comma in object"));
            }
        }
        Ok(Value::Object(values))
    }

    fn array(&mut self, depth: usize) -> Result<Value, SyntaxError> {
        self.expect_byte(b'[')?;
        let mut values = Vec::new();
        self.whitespace();
        if self.consume_byte(b']') {
            return Ok(Value::Array(values));
        }
        loop {
            values.push(self.value(depth)?);
            self.whitespace();
            if self.consume_byte(b']') {
                break;
            }
            self.expect_byte(b',')?;
            self.whitespace();
            if self.next_is(b']') {
                return Err(self.syntax("trailing comma in array"));
            }
        }
        Ok(Value::Array(values))
    }

    fn number(&mut self) -> Result<u128, SyntaxError> {
        let bytes = self.source.as_bytes();
        let start = self.offset;
        if bytes.get(self.offset) == Some(&b'0') {
            self.offset += 1;
            if bytes.get(self.offset).is_some_and(u8::is_ascii_digit) {
                return Err(self.syntax("leading zero in number"));
            }
            return Ok(0);
        }
        while bytes.get(self.offset).is_some_and(u8::is_ascii_digit) {
            self.offset += 1;
        }
        if self.offset == start {
            return Err(self.syntax("expected unsigned integer"));
        }
        self.source[start..self.offset]
            .parse()
            .map_err(|_| self.syntax("unsigned integer overflow"))
    }

    fn string(&mut self) -> Result<String, SyntaxError> {
        self.expect_byte(b'"')?;
        let mut output = String::new();
        loop {
            let Some(byte) = self.source.as_bytes().get(self.offset).copied() else {
                return Err(self.syntax("unterminated string"));
            };
            match byte {
                b'"' => {
                    self.offset += 1;
                    return Ok(output);
                }
                b'\\' => {
                    self.offset += 1;
                    self.escape(&mut output)?;
                }
                0..=0x1f => return Err(self.syntax("unescaped control character")),
                _ => {
                    let character = self
                        .remaining()
                        .chars()
                        .next()
                        .ok_or_else(|| self.syntax("unterminated string"))?;
                    output.push(character);
                    self.offset += character.len_utf8();
                }
            }
        }
    }

    fn escape(&mut self, output: &mut String) -> Result<(), SyntaxError> {
        let Some(escaped) = self.source.as_bytes().get(self.offset).copied() else {
            return Err(self.syntax("unterminated escape"));
        };
        self.offset += 1;
        match escaped {
            b'"' => output.push('"'),
            b'\\' => output.push('\\'),
            b'/' => output.push('/'),
            b'b' => output.push('\u{08}'),
            b'f' => output.push('\u{0c}'),
            b'n' => output.push('\n'),
            b'r' => output.push('\r'),
            b't' => output.push('\t'),
            b'u' => self.unicode_escape(output)?,
            _ => return Err(self.syntax("invalid string escape")),
        }
        Ok(())
    }

    fn unicode_escape(&mut self, output: &mut String) -> Result<(), SyntaxError> {
        let first = self.hex_quad()?;
        let scalar = if (0xd800..=0xdbff).contains(&first) {
            if !self.remaining().starts_with("\\u") {
                return Err(self.syntax("high surrogate without low surrogate"));
            }
            self.offset += 2;
            let second = self.hex_quad()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return Err(self.syntax("invalid low surrogate"));
            }
            0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
        } else if (0xdc00..=0xdfff).contains(&first) {
            return Err(self.syntax("unexpected low surrogate"));
        } else {
            u32::from(first)
        };
        output.push(char::from_u32(scalar).ok_or_else(|| self.syntax("invalid Unicode scalar"))?);
        Ok(())
    }

    fn hex_quad(&mut self) -> Result<u16, SyntaxError> {
        let end = self.offset.saturating_add(4);
        let Some(value) = self.source.get(self.offset..end) else {
            return Err(self.syntax("truncated Unicode escape"));
        };
        if !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(self.syntax("invalid Unicode escape"));
        }
        self.offset = end;
        u16::from_str_radix(value, 16).map_err(|_| self.syntax("invalid Unicode escape"))
    }

    fn whitespace(&mut self) {
        while self
            .source
            .as_bytes()
            .get(self.offset)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.offset += 1;
        }
    }

    fn expect_byte(&mut self, expected: u8) -> Result<(), SyntaxError> {
        if self.consume_byte(expected) {
            Ok(())
        } else {
            Err(self.syntax(format!("expected `{}`", char::from(expected))))
        }
    }

    fn consume_byte(&mut self, expected: u8) -> bool {
        if self.next_is(expected) {
            self.offset += 1;
            true
        } else {
            false
        }
    }

    fn next_is(&self, expected: u8) -> bool {
        self.source.as_bytes().get(self.offset) == Some(&expected)
    }

    fn consume_literal(&mut self, literal: &str) -> bool {
        if self.remaining().starts_with(literal) {
            self.offset += literal.len();
            true
        } else {
            false
        }
    }

    fn remaining(&self) -> &str {
        &self.source[self.offset..]
    }

    fn syntax(&self, message: impl Into<String>) -> SyntaxError {
        SyntaxError {
            offset: self.offset,
            message: message.into(),
        }
    }
}

/// An object whose fields are taken one by one; [`Self::finish`] refuses
/// any left over.
pub struct Object {
    context: &'static str,
    values: BTreeMap<String, Value>,
}

impl Object {
    /// # Errors
    ///
    /// [`SyntaxError`] when `value` is not an object.
    pub fn new(value: Value, context: &'static str) -> Result<Self, SyntaxError> {
        let Value::Object(values) = value else {
            return Err(syntax(format!("{context} must be an object")));
        };
        Ok(Self { context, values })
    }

    /// # Errors
    ///
    /// [`SyntaxError`] when the field is missing.
    pub fn take(&mut self, field: &str) -> Result<Value, SyntaxError> {
        self.values
            .remove(field)
            .ok_or_else(|| syntax(format!("missing field `{field}` in {}", self.context)))
    }

    /// # Errors
    ///
    /// [`SyntaxError`] when the field is missing or not a string.
    pub fn string(&mut self, field: &str) -> Result<String, SyntaxError> {
        let Value::String(value) = self.take(field)? else {
            return Err(syntax(format!("field `{field}` must be a string")));
        };
        Ok(value)
    }

    /// # Errors
    ///
    /// [`SyntaxError`] when the field is missing or not a boolean.
    pub fn boolean(&mut self, field: &str) -> Result<bool, SyntaxError> {
        let Value::Bool(value) = self.take(field)? else {
            return Err(syntax(format!("field `{field}` must be a boolean")));
        };
        Ok(value)
    }

    fn number(&mut self, field: &str) -> Result<u128, SyntaxError> {
        let Value::Number(value) = self.take(field)? else {
            return Err(syntax(format!(
                "field `{field}` must be an unsigned integer"
            )));
        };
        Ok(value)
    }

    /// # Errors
    ///
    /// [`SyntaxError`] when the field is missing or not a `u16`.
    pub fn u16(&mut self, field: &str) -> Result<u16, SyntaxError> {
        u16::try_from(self.number(field)?)
            .map_err(|_| syntax(format!("field `{field}` exceeds u16")))
    }

    /// # Errors
    ///
    /// [`SyntaxError`] when the field is missing or not a `u32`.
    pub fn u32(&mut self, field: &str) -> Result<u32, SyntaxError> {
        u32::try_from(self.number(field)?)
            .map_err(|_| syntax(format!("field `{field}` exceeds u32")))
    }

    /// # Errors
    ///
    /// [`SyntaxError`] when the field is missing or not an `i16`.
    pub fn i16(&mut self, field: &str) -> Result<i16, SyntaxError> {
        i16::try_from(self.i64(field)?).map_err(|_| syntax(format!("field `{field}` exceeds i16")))
    }

    /// # Errors
    ///
    /// [`SyntaxError`] when the field is missing or not an `i64`.
    pub fn i64(&mut self, field: &str) -> Result<i64, SyntaxError> {
        match self.take(field)? {
            Value::Number(value) => {
                i64::try_from(value).map_err(|_| syntax(format!("field `{field}` exceeds i64")))
            }
            Value::NegativeNumber(value) => {
                let magnitude = i128::try_from(value)
                    .map_err(|_| syntax(format!("field `{field}` exceeds i64")))?;
                i64::try_from(-magnitude)
                    .map_err(|_| syntax(format!("field `{field}` exceeds i64")))
            }
            _ => Err(syntax(format!("field `{field}` must be an integer"))),
        }
    }

    /// # Errors
    ///
    /// [`SyntaxError`] naming the first field nothing took.
    pub fn finish(self) -> Result<(), SyntaxError> {
        if let Some(field) = self.values.keys().next() {
            Err(syntax(format!(
                "unknown field `{field}` in {}",
                self.context
            )))
        } else {
            Ok(())
        }
    }
}

/// Parses every element of an array.
///
/// # Errors
///
/// [`SyntaxError`] when `value` is not an array, or `parse`'s first error.
pub fn parse_array<T, E: From<SyntaxError>>(
    value: Value,
    context: &'static str,
    parse: impl FnMut(Value) -> Result<T, E>,
) -> Result<Vec<T>, E> {
    let Value::Array(values) = value else {
        return Err(syntax(format!("{context} must be an array")).into());
    };
    values.into_iter().map(parse).collect()
}

/// `None` for `null`, otherwise `parse(value)`.
///
/// # Errors
///
/// `parse`'s error.
pub fn parse_optional<T, E>(
    value: Value,
    parse: impl FnOnce(Value) -> Result<T, E>,
) -> Result<Option<T>, E> {
    if matches!(value, Value::Null) {
        Ok(None)
    } else {
        parse(value).map(Some)
    }
}

/// Appends `value` JSON-escaped (without quotes).
pub fn escape_string(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                write!(output, "\\u{:04x}", u32::from(character))
                    .expect("writing to a string cannot fail");
            }
            character => output.push(character),
        }
    }
}

/// Parses `#rrggbbaa` (hex digits in either case) into its four channels.
///
/// # Errors
///
/// [`SyntaxError`] for any other text.
pub fn parse_hex_rgba(value: &str) -> Result<[u8; 4], SyntaxError> {
    let digits = value
        .strip_prefix('#')
        .filter(|digits| digits.len() == 8 && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| syntax(format!("color `{value}` must be #rrggbbaa")))?;
    let channel = |index: usize| {
        u8::from_str_radix(&digits[index * 2..index * 2 + 2], 16)
            .map_err(|_| syntax(format!("color `{value}` must be #rrggbbaa")))
    };
    Ok([channel(0)?, channel(1)?, channel(2)?, channel(3)?])
}

/// Writes `#rrggbbaa` in lowercase hex.
pub fn write_hex_rgba(output: &mut String, [red, green, blue, alpha]: [u8; 4]) {
    write!(output, "#{red:02x}{green:02x}{blue:02x}{alpha:02x}")
        .expect("writing to a string cannot fail");
}
