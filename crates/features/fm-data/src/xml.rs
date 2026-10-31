//! A bounded, deterministic XML reader (GX-004).
//!
//! The parser accepts one root element with attributes, text, nested
//! elements, comments, processing instructions, and CDATA sections. It
//! rejects a DOCTYPE (so no entity expansion can blow up), duplicate
//! attributes, mismatched closing tags, and trailing content. Every bound
//! comes from [`ParseLimits`], and entities are limited to the five XML
//! named entities plus numeric character references.
//!
//! An element becomes [`DataValue::Object`] with `tag`, `attributes`, and
//! `children` keys; each child is either a [`DataValue::String`] text run or
//! another element object, in document order.

use crate::parse::{ParseError, ParseErrorKind, ParseLimits, decode};
use crate::value::DataValue;
use std::collections::BTreeMap;

/// Parses one XML document into an exact [`DataValue`].
///
/// # Errors
///
/// Returns a located [`ParseError`] for malformed XML or limit violations.
pub fn parse_xml(bytes: &[u8], limits: &ParseLimits) -> Result<DataValue, ParseError> {
    let text = decode(bytes, limits)?;
    let mut parser = XmlParser {
        text,
        bytes: text.as_bytes(),
        pos: 0,
        nodes: 0,
        limits,
    };
    parser.skip_misc()?;
    let root = parser.element(0)?;
    parser.skip_misc()?;
    if parser.pos != parser.bytes.len() {
        return Err(parser.error(ParseErrorKind::TrailingData));
    }
    Ok(root)
}

struct XmlParser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    nodes: usize,
    limits: &'a ParseLimits,
}

impl XmlParser<'_> {
    fn error(&self, kind: ParseErrorKind) -> ParseError {
        ParseError::at(self.text, self.pos, kind)
    }

    fn starts_with(&self, prefix: &str) -> bool {
        self.bytes[self.pos..].starts_with(prefix.as_bytes())
    }

    fn skip_whitespace(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
    }

    /// Skips whitespace, comments, and processing instructions between
    /// top-level constructs.
    fn skip_misc(&mut self) -> Result<(), ParseError> {
        loop {
            self.skip_whitespace();
            if self.starts_with("<?") {
                let Some(end) = self.text[self.pos..].find("?>") else {
                    return Err(self.error(ParseErrorKind::UnexpectedEnd));
                };
                self.pos += end + 2;
            } else if self.starts_with("<!--") {
                let Some(end) = self.text[self.pos + 4..].find("-->") else {
                    return Err(self.error(ParseErrorKind::UnexpectedEnd));
                };
                self.pos += 4 + end + 3;
            } else if self.starts_with("<!DOCTYPE") || self.starts_with("<!doctype") {
                return Err(self.error(ParseErrorKind::UnsupportedXmlDoctype));
            } else {
                return Ok(());
            }
        }
    }

    fn count_node(&mut self) -> Result<(), ParseError> {
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > self.limits.max_nodes {
            return Err(self.error(ParseErrorKind::TooManyNodes {
                limit: self.limits.max_nodes,
            }));
        }
        Ok(())
    }

    fn element(&mut self, depth: usize) -> Result<DataValue, ParseError> {
        if depth > self.limits.max_depth {
            return Err(self.error(ParseErrorKind::DepthExceeded {
                limit: self.limits.max_depth,
            }));
        }
        if !self.starts_with("<") || self.starts_with("</") || self.starts_with("<!") {
            return Err(self.error(ParseErrorKind::InvalidXml {
                reason: "expected an element",
            }));
        }
        self.pos += 1;
        self.count_node()?;
        let tag = self.name()?;
        let mut attributes = BTreeMap::new();
        loop {
            self.skip_whitespace();
            if self.starts_with("/>") {
                self.pos += 2;
                return Ok(element_value(tag, attributes, Vec::new()));
            }
            if self.starts_with(">") {
                self.pos += 1;
                break;
            }
            let attribute = self.name()?;
            self.skip_whitespace();
            if !self.starts_with("=") {
                return Err(self.error(ParseErrorKind::InvalidXml {
                    reason: "attribute without a value",
                }));
            }
            self.pos += 1;
            self.skip_whitespace();
            let value = self.quoted()?;
            if attributes.insert(attribute, value).is_some() {
                return Err(self.error(ParseErrorKind::DuplicateKey(
                    "duplicate attribute".to_owned(),
                )));
            }
            self.count_node()?;
        }
        let mut children = Vec::new();
        loop {
            if self.pos >= self.bytes.len() {
                return Err(self.error(ParseErrorKind::UnexpectedEnd));
            }
            if self.starts_with("<!--") {
                let Some(end) = self.text[self.pos + 4..].find("-->") else {
                    return Err(self.error(ParseErrorKind::UnexpectedEnd));
                };
                self.pos += 4 + end + 3;
            } else if self.starts_with("<![CDATA[") {
                let Some(end) = self.text[self.pos + 9..].find("]]>") else {
                    return Err(self.error(ParseErrorKind::UnexpectedEnd));
                };
                let text = &self.text[self.pos + 9..self.pos + 9 + end];
                if !text.is_empty() {
                    self.count_node()?;
                    children.push(DataValue::String(text.to_owned()));
                }
                self.pos += 9 + end + 3;
            } else if self.starts_with("</") {
                self.pos += 2;
                let close = self.name()?;
                self.skip_whitespace();
                if close != tag || !self.starts_with(">") {
                    return Err(self.error(ParseErrorKind::InvalidXml {
                        reason: "mismatched closing tag",
                    }));
                }
                self.pos += 1;
                return Ok(element_value(tag, attributes, children));
            } else if self.starts_with("<") {
                children.push(self.element(depth + 1)?);
            } else {
                let start = self.pos;
                let end = self.text[start..]
                    .find('<')
                    .map_or(self.bytes.len(), |end| start + end);
                let raw = &self.text[start..end];
                self.pos = end;
                let text = decode_entities(raw, start, self.text)?;
                if text.len() > self.limits.max_field_bytes {
                    return Err(self.error(ParseErrorKind::FieldTooLarge {
                        limit: self.limits.max_field_bytes,
                    }));
                }
                // Indentation between elements is not data; mixed-content
                // text that carries anything else is kept verbatim.
                if !text.trim().is_empty() {
                    self.count_node()?;
                    children.push(DataValue::String(text));
                }
            }
        }
    }

    fn name(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        let first = self
            .bytes
            .get(self.pos)
            .copied()
            .ok_or_else(|| self.error(ParseErrorKind::UnexpectedEnd))?;
        if !(first.is_ascii_alphabetic() || matches!(first, b'_' | b':')) {
            return Err(self.error(ParseErrorKind::InvalidXml {
                reason: "invalid name",
            }));
        }
        self.pos += 1;
        while self.bytes.get(self.pos).is_some_and(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-' | b'.')
        }) {
            self.pos += 1;
        }
        let name = &self.text[start..self.pos];
        if name.len() > self.limits.max_field_bytes {
            return Err(self.error(ParseErrorKind::FieldTooLarge {
                limit: self.limits.max_field_bytes,
            }));
        }
        Ok(name.to_owned())
    }

    fn quoted(&mut self) -> Result<String, ParseError> {
        let quote = self
            .bytes
            .get(self.pos)
            .copied()
            .filter(|byte| matches!(byte, b'"' | b'\''))
            .ok_or_else(|| {
                self.error(ParseErrorKind::InvalidXml {
                    reason: "value is not quoted",
                })
            })?;
        let start = self.pos + 1;
        let end = self.text[start..]
            .find(char::from(quote))
            .map(|end| start + end)
            .ok_or_else(|| self.error(ParseErrorKind::UnexpectedEnd))?;
        let value = decode_entities(&self.text[start..end], start, self.text)?;
        if value.len() > self.limits.max_field_bytes {
            return Err(self.error(ParseErrorKind::FieldTooLarge {
                limit: self.limits.max_field_bytes,
            }));
        }
        self.pos = end + 1;
        Ok(value)
    }
}

fn element_value(
    tag: String,
    attributes: BTreeMap<String, String>,
    children: Vec<DataValue>,
) -> DataValue {
    let mut object = BTreeMap::new();
    object.insert("tag".to_owned(), DataValue::String(tag));
    object.insert(
        "attributes".to_owned(),
        DataValue::Object(
            attributes
                .into_iter()
                .map(|(name, value)| (name, DataValue::String(value)))
                .collect(),
        ),
    );
    object.insert("children".to_owned(), DataValue::List(children));
    DataValue::Object(object)
}

/// Decodes the five XML named entities and numeric character references.
pub(crate) fn decode_entities(
    value: &str,
    offset: usize,
    text: &str,
) -> Result<String, ParseError> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        let Some(end) = rest[index..].find(';') else {
            return Err(ParseError::at(
                text,
                offset + value.len() - rest.len() + index,
                ParseErrorKind::InvalidEscape,
            ));
        };
        let entity = &rest[index + 1..index + end];
        let decoded = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = if let Some(hex) = entity.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(decimal) = entity.strip_prefix('#') {
                    decimal.parse::<u32>().ok()
                } else {
                    None
                };
                let Some(character) = code.and_then(char::from_u32) else {
                    return Err(ParseError::at(
                        text,
                        offset + value.len() - rest.len() + index,
                        ParseErrorKind::InvalidEscape,
                    ));
                };
                character
            }
        };
        out.push(decoded);
        rest = &rest[index + end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: ParseLimits = ParseLimits {
        max_input_bytes: 64 * 1024,
        max_rows: 100,
        max_columns: 16,
        max_field_bytes: 1024,
        max_depth: 8,
        max_nodes: 100,
    };

    #[test]
    fn elements_attributes_and_text_become_objects_in_order() {
        let document = parse_xml(
            br#"<?xml version="1.0"?>
                <show name="Demo &amp; Co">
                  <item id="1">One</item>
                  <item id="2"><![CDATA[Two & more]]></item>
                  <!-- a comment -->
                </show>"#,
            &LIMITS,
        )
        .unwrap();
        let DataValue::Object(show) = document else {
            panic!("expected an element");
        };
        assert_eq!(show.get("tag"), Some(&DataValue::String("show".to_owned())));
        assert_eq!(
            show.get("attributes"),
            Some(&DataValue::Object(
                [("name".to_owned(), DataValue::String("Demo & Co".to_owned()))]
                    .into_iter()
                    .collect()
            ))
        );
        let Some(DataValue::List(children)) = show.get("children") else {
            panic!("expected children");
        };
        assert_eq!(children.len(), 2);
        let DataValue::Object(first) = &children[0] else {
            panic!("expected an element");
        };
        assert_eq!(
            first.get("children"),
            Some(&DataValue::List(vec![DataValue::String("One".to_owned())]))
        );
        let DataValue::Object(second) = &children[1] else {
            panic!("expected an element");
        };
        assert_eq!(
            second.get("children"),
            Some(&DataValue::List(vec![DataValue::String(
                "Two & more".to_owned()
            )]))
        );
    }

    #[test]
    fn malformed_documents_are_refused_without_limits() {
        for document in [
            "<a><b></a>",
            "<a b='1' b='2'/>",
            "<a>text</a><b/>",
            "<!DOCTYPE a [<!ENTITY x 'y'>]><a/>",
            "<a b=1/>",
            "<a>&unknown;</a>",
        ] {
            assert!(
                parse_xml(document.as_bytes(), &LIMITS).is_err(),
                "{document}"
            );
        }
    }

    #[test]
    fn depth_and_node_bounds_hold() {
        let deep = "<a><b><c/></b></a>";
        let mut limits = LIMITS;
        limits.max_depth = 1;
        assert!(matches!(
            parse_xml(deep.as_bytes(), &limits),
            Err(ParseError {
                kind: ParseErrorKind::DepthExceeded { .. },
                ..
            })
        ));
        let mut limits = LIMITS;
        limits.max_nodes = 2;
        assert!(matches!(
            parse_xml(b"<a x='1' y='2'/>", &limits),
            Err(ParseError {
                kind: ParseErrorKind::TooManyNodes { .. },
                ..
            })
        ));
    }
}
