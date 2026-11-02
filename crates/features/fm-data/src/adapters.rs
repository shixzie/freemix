use std::{collections::BTreeMap, fmt};

use crate::xpath::{child_elements, element_attribute_map, element_tag, element_text};
use crate::{DataValue, XPathError, select_xml};

/// Converts an adapter-specific representation into typed data.
pub trait DataAdapter {
    /// Produces the adapter's typed representation.
    ///
    /// # Errors
    ///
    /// Returns an error when the in-memory representation is malformed.
    fn representation(&self) -> Result<DataValue, AdapterError>;
}

/// An in-memory adapter representation is malformed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdapterError {
    DuplicateHeader(String),
    RowWidth {
        row: usize,
        expected: usize,
        actual: usize,
    },
    ExpectedObject,
    /// The row `XPath` selected an attribute or text node, not an element.
    RowNotElement,
    /// One selected element holds more distinct fields than the adapter
    /// accepts.
    TooManyRowFields {
        field: usize,
        maximum: usize,
    },
    /// The row expression is not valid for [`select_xml`].
    XPath(XPathError),
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateHeader(header) => write!(formatter, "duplicate CSV header `{header}`"),
            Self::RowWidth {
                row,
                expected,
                actual,
            } => write!(
                formatter,
                "CSV row {row} has {actual} columns; expected {expected}"
            ),
            Self::ExpectedObject => formatter.write_str("JSON-like adapter root must be an object"),
            Self::RowNotElement => {
                formatter.write_str("XML row XPath must select elements, not attributes or text")
            }
            Self::TooManyRowFields { field, maximum } => write!(
                formatter,
                "XML row field {field} exceeds the {maximum}-field adapter limit"
            ),
            Self::XPath(error) => write!(formatter, "XML row XPath: {error}"),
        }
    }
}

impl std::error::Error for AdapterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::XPath(error) => Some(error),
            _ => None,
        }
    }
}

/// Most distinct fields one selected XML element may contribute to a row.
pub const MAX_XML_ROW_FIELDS: usize = 256;

/// Turns XPath-selected XML elements into typed rows for the title mapper.
///
/// Each selected element becomes an object: attributes as `@name`, direct
/// child elements by tag (repeated tags become a list of strings), and - when
/// the element has no attributes or child elements - its own text under
/// `_text`. Text includes inline descendants, so `<title>A <b>B</b></title>`
/// yields `AB` with a space from the source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XmlRowsAdapter {
    document: DataValue,
    row_xpath: String,
}

impl XmlRowsAdapter {
    /// Creates an adapter over an already parsed XML document.
    #[must_use]
    pub fn new(document: DataValue, row_xpath: impl Into<String>) -> Self {
        Self {
            document,
            row_xpath: row_xpath.into(),
        }
    }

    /// The parsed document the adapter selects from.
    #[must_use]
    pub const fn document(&self) -> &DataValue {
        &self.document
    }

    /// The `XPath` expression that selects one element per row.
    #[must_use]
    pub fn row_xpath(&self) -> &str {
        &self.row_xpath
    }
}

impl DataAdapter for XmlRowsAdapter {
    fn representation(&self) -> Result<DataValue, AdapterError> {
        let mut rows = Vec::new();
        for element in select_xml(&self.document, &self.row_xpath).map_err(AdapterError::XPath)? {
            let fields = row_fields(&element)?;
            rows.push(DataValue::Object(fields));
        }
        Ok(DataValue::List(rows))
    }
}

fn row_fields(element: &DataValue) -> Result<BTreeMap<String, DataValue>, AdapterError> {
    if element_tag(element).is_none() {
        return Err(AdapterError::RowNotElement);
    }
    let mut fields = BTreeMap::new();
    if let Some(attributes) = element_attribute_map(element) {
        for (name, value) in attributes {
            if let DataValue::String(text) = value {
                fields.insert(format!("@{name}"), DataValue::String(text.clone()));
            }
        }
    }
    for child in child_elements(element) {
        let Some(tag) = element_tag(child) else {
            continue;
        };
        let text = DataValue::String(element_text(child));
        match fields.entry(tag.to_owned()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(text);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => match entry.get_mut() {
                DataValue::List(values) => values.push(text),
                existing => {
                    let first = std::mem::replace(existing, DataValue::Null);
                    *existing = DataValue::List(vec![first, text]);
                }
            },
        }
    }
    if fields.len() > MAX_XML_ROW_FIELDS {
        return Err(AdapterError::TooManyRowFields {
            field: fields.len(),
            maximum: MAX_XML_ROW_FIELDS,
        });
    }
    if fields.is_empty() {
        let text = element_text(element);
        if !text.is_empty() {
            fields.insert("_text".to_owned(), DataValue::String(text));
        }
    }
    Ok(fields)
}

/// CSV-like headers and rows. Parsing and I/O are intentionally out of scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CsvRowsAdapter {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl CsvRowsAdapter {
    #[must_use]
    pub fn new(headers: Vec<String>, rows: Vec<Vec<String>>) -> Self {
        Self { headers, rows }
    }
}

impl DataAdapter for CsvRowsAdapter {
    fn representation(&self) -> Result<DataValue, AdapterError> {
        let mut seen = BTreeMap::new();
        for header in &self.headers {
            if seen.insert(header, ()).is_some() {
                return Err(AdapterError::DuplicateHeader(header.clone()));
            }
        }
        let mut output = Vec::with_capacity(self.rows.len());
        for (index, row) in self.rows.iter().enumerate() {
            if row.len() != self.headers.len() {
                return Err(AdapterError::RowWidth {
                    row: index,
                    expected: self.headers.len(),
                    actual: row.len(),
                });
            }
            let object = self
                .headers
                .iter()
                .cloned()
                .zip(row.iter().cloned().map(DataValue::String))
                .collect();
            output.push(DataValue::Object(object));
        }
        Ok(DataValue::List(output))
    }
}

/// A pre-parsed JSON-like object representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonObjectAdapter {
    object: BTreeMap<String, DataValue>,
}

impl JsonObjectAdapter {
    #[must_use]
    pub fn new(object: BTreeMap<String, DataValue>) -> Self {
        Self { object }
    }

    /// Builds an adapter from an object value.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError::ExpectedObject`] for any non-object value.
    pub fn try_from_value(value: DataValue) -> Result<Self, AdapterError> {
        let DataValue::Object(object) = value else {
            return Err(AdapterError::ExpectedObject);
        };
        Ok(Self { object })
    }

    #[must_use]
    pub const fn object(&self) -> &BTreeMap<String, DataValue> {
        &self.object
    }
}

impl DataAdapter for JsonObjectAdapter {
    fn representation(&self) -> Result<DataValue, AdapterError> {
        Ok(DataValue::Object(self.object.clone()))
    }
}
