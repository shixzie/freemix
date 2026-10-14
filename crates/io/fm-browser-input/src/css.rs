//! Structural validation for operator-supplied custom CSS.
//!
//! The validator is a small lexical scan, not a CSS parser. It rejects input
//! that could escape a `<style>` element, fetch through `@import`, or hide
//! function names behind CSS escapes, and it extracts `url(...)` references so
//! [`crate::NetworkPolicy::check_css`] can apply navigation policy to them.

use core::fmt;

use crate::BrowserLimits;

/// Why custom CSS was rejected. Offsets are byte offsets into the input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CssError {
    TooLarge { actual: usize, maximum: usize },
    StyleBreakout { offset: usize },
    ControlCharacter { offset: usize },
    EscapeOutsideString { offset: usize },
    Import { offset: usize },
    UnsupportedFunction { offset: usize, name: String },
    UnterminatedComment { offset: usize },
    UnterminatedString { offset: usize },
    InvalidUrlReference { offset: usize },
    RelativeUrlReference { offset: usize },
    TooManyReferences { maximum: usize },
}

impl fmt::Display for CssError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual, maximum } => {
                write!(formatter, "custom CSS bytes {actual} exceeds {maximum}")
            }
            Self::StyleBreakout { offset } => {
                write!(
                    formatter,
                    "custom CSS closes its style element at byte {offset}"
                )
            }
            Self::ControlCharacter { offset } => {
                write!(
                    formatter,
                    "custom CSS has a control character at byte {offset}"
                )
            }
            Self::EscapeOutsideString { offset } => {
                write!(
                    formatter,
                    "custom CSS has an escape outside a string at byte {offset}"
                )
            }
            Self::Import { offset } => {
                write!(formatter, "custom CSS uses @import at byte {offset}")
            }
            Self::UnsupportedFunction { offset, name } => {
                write!(
                    formatter,
                    "custom CSS uses unsupported {name}() at byte {offset}"
                )
            }
            Self::UnterminatedComment { offset } => {
                write!(
                    formatter,
                    "custom CSS comment at byte {offset} is unterminated"
                )
            }
            Self::UnterminatedString { offset } => {
                write!(
                    formatter,
                    "custom CSS string at byte {offset} is unterminated"
                )
            }
            Self::InvalidUrlReference { offset } => {
                write!(formatter, "custom CSS url() at byte {offset} is malformed")
            }
            Self::RelativeUrlReference { offset } => {
                write!(formatter, "custom CSS url() at byte {offset} is relative")
            }
            Self::TooManyReferences { maximum } => {
                write!(
                    formatter,
                    "custom CSS has more than {maximum} external url() references"
                )
            }
        }
    }
}

impl std::error::Error for CssError {}

/// CSS which passed structural validation, with its external references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomCss {
    text: String,
    external_urls: Vec<String>,
    inline_references: usize,
}

impl CustomCss {
    /// Maximum external `url(...)` references per stylesheet.
    pub const MAX_EXTERNAL_REFERENCES: usize = 32;

    /// Validates custom CSS against size and structural rules.
    ///
    /// `data:` and `#fragment` references are counted as inline; any other
    /// reference must be an absolute URL and is returned for policy checks.
    ///
    /// # Errors
    ///
    /// Returns the first [`CssError`] found, scanning left to right.
    pub fn parse(text: impl Into<String>, limits: BrowserLimits) -> Result<Self, CssError> {
        let text = text.into();
        if text.len() > limits.max_css_bytes {
            return Err(CssError::TooLarge {
                actual: text.len(),
                maximum: limits.max_css_bytes,
            });
        }
        let bytes = text.as_bytes();
        if let Some(offset) = find_ignore_case(bytes, b"</style", 0) {
            return Err(CssError::StyleBreakout { offset });
        }
        if let Some(offset) = bytes.iter().position(|byte| {
            byte.is_ascii_control() && !matches!(byte, b'\t' | b'\n' | b'\r' | 0x0c)
        }) {
            return Err(CssError::ControlCharacter { offset });
        }

        let mut external_urls = Vec::new();
        let mut inline_references = 0_usize;
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
                let end = find(bytes, b"*/", index + 2)
                    .ok_or(CssError::UnterminatedComment { offset: index })?;
                index = end + 2;
            } else if matches!(byte, b'"' | b'\'') {
                index = skip_string(bytes, index)?;
            } else if byte == b'\\' {
                return Err(CssError::EscapeOutsideString { offset: index });
            } else if byte == b'@' {
                let end = ident_end(bytes, index + 1);
                if text[index + 1..end].eq_ignore_ascii_case("import") {
                    return Err(CssError::Import { offset: index });
                }
                index = end.max(index + 1);
            } else if is_ident_start(byte) {
                let end = ident_end(bytes, index);
                let name = &text[index..end];
                if bytes.get(end) == Some(&b'(') {
                    if name.eq_ignore_ascii_case("url") {
                        let (reference, next) = url_argument(&text, end + 1, index)?;
                        let inline = reference
                            .as_bytes()
                            .get(..5)
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"data:"))
                            || reference.starts_with('#');
                        if inline {
                            inline_references += 1;
                        } else if crate::contract::split_scheme(reference).is_some() {
                            if external_urls.len() >= Self::MAX_EXTERNAL_REFERENCES {
                                return Err(CssError::TooManyReferences {
                                    maximum: Self::MAX_EXTERNAL_REFERENCES,
                                });
                            }
                            external_urls.push(reference.to_owned());
                        } else {
                            return Err(CssError::RelativeUrlReference { offset: index });
                        }
                        index = next;
                        continue;
                    }
                    let lower = name.to_ascii_lowercase();
                    if matches!(
                        lower.as_str(),
                        "image-set" | "-webkit-image-set" | "src" | "expression" | "image"
                    ) {
                        return Err(CssError::UnsupportedFunction {
                            offset: index,
                            name: lower,
                        });
                    }
                }
                index = end;
            } else {
                index += 1;
            }
        }
        Ok(Self {
            text,
            external_urls,
            inline_references,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.text
    }

    /// Absolute external references, in source order.
    #[must_use]
    pub fn external_urls(&self) -> &[String] {
        &self.external_urls
    }

    /// Count of `data:` and `#fragment` references.
    #[must_use]
    pub const fn inline_references(&self) -> usize {
        self.inline_references
    }
}

const fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'-' | b'_')
}

fn ident_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'-' | b'_'))
    {
        end += 1;
    }
    end
}

fn skip_string(bytes: &[u8], start: usize) -> Result<usize, CssError> {
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'\n' | b'\r' | 0x0c => return Err(CssError::UnterminatedString { offset: start }),
            byte if byte == quote => return Ok(index + 1),
            _ => index += 1,
        }
    }
    Err(CssError::UnterminatedString { offset: start })
}

/// Returns the trimmed reference and the index after the closing `)`.
fn url_argument(text: &str, start: usize, offset: usize) -> Result<(&str, usize), CssError> {
    let bytes = text.as_bytes();
    let invalid = CssError::InvalidUrlReference { offset };
    let skip_whitespace = |mut index: usize| {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        index
    };
    let index = skip_whitespace(start);
    let (reference, after) = match bytes.get(index) {
        Some(&quote @ (b'"' | b'\'')) => {
            let end = bytes[index + 1..]
                .iter()
                .position(|byte| *byte == quote)
                .map(|position| index + 1 + position)
                .ok_or(CssError::UnterminatedString { offset: index })?;
            let reference = &text[index + 1..end];
            if reference
                .bytes()
                .any(|byte| matches!(byte, b'\\' | b'\n' | b'\r' | 0x0c))
            {
                return Err(invalid);
            }
            (reference, skip_whitespace(end + 1))
        }
        Some(_) => {
            let mut end = index;
            while end < bytes.len() && !bytes[end].is_ascii_whitespace() && bytes[end] != b')' {
                if matches!(bytes[end], b'"' | b'\'' | b'(' | b'\\') {
                    return Err(invalid);
                }
                end += 1;
            }
            (&text[index..end], skip_whitespace(end))
        }
        None => return Err(invalid),
    };
    if bytes.get(after) != Some(&b')') || reference.is_empty() {
        return Err(invalid);
    }
    Ok((reference, after + 1))
}

fn find(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    haystack
        .get(start..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| start + position)
}

fn find_ignore_case(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    haystack
        .get(start..)?
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
        .map(|position| start + position)
}
