//! RSS 2.0 and Atom feed reading on top of [`crate::parse_xml`] (GX-004).
//!
//! A feed becomes one [`DataValue::Object`] per item with scalar fields:
//! `title`, `link`, `summary`, `published`, and `id`. RSS `item`s and Atom
//! `entry`s are both accepted; missing fields are `DataValue::Null`. The
//! item count is bounded by [`ParseLimits::max_rows`].

use crate::parse::{ParseError, ParseLimits};
use crate::value::DataValue;
use crate::xml::parse_xml;
use std::fmt;

/// Why a feed could not be read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FeedError {
    /// The underlying XML is malformed or exceeds a parse limit.
    Parse(ParseError),
    /// The document is XML but neither RSS 2.0 nor Atom.
    NotAFeed,
    /// The feed holds more items than [`ParseLimits::max_rows`].
    TooManyItems { limit: usize },
}

impl fmt::Display for FeedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => error.fmt(formatter),
            Self::NotAFeed => {
                formatter.write_str("the document is neither an RSS nor an Atom feed")
            }
            Self::TooManyItems { limit } => {
                write!(formatter, "the feed holds more than {limit} items")
            }
        }
    }
}

impl std::error::Error for FeedError {}

impl From<ParseError> for FeedError {
    fn from(error: ParseError) -> Self {
        Self::Parse(error)
    }
}

/// Reads the items of one RSS 2.0 or Atom feed.
///
/// # Errors
///
/// Returns [`FeedError`] for malformed XML, a document that is not a feed, or
/// too many items.
pub fn parse_feed(bytes: &[u8], limits: &ParseLimits) -> Result<Vec<DataValue>, FeedError> {
    let document = parse_xml(bytes, limits)?;
    let root = tag(&document).ok_or(FeedError::NotAFeed)?;
    let (items, atom) = match root {
        "rss" => {
            let channel = children(&document)
                .into_iter()
                .find(|child| tag(child) == Some("channel"))
                .ok_or(FeedError::NotAFeed)?;
            (named_children(channel, "item"), false)
        }
        "feed" => (named_children(&document, "entry"), true),
        _ => return Err(FeedError::NotAFeed),
    };
    if items.len() > limits.max_rows {
        return Err(FeedError::TooManyItems {
            limit: limits.max_rows,
        });
    }
    Ok(items.into_iter().map(|item| entry(item, atom)).collect())
}

fn entry(item: &DataValue, atom: bool) -> DataValue {
    let field = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| child_text(item, name))
            .map_or(DataValue::Null, DataValue::String)
    };
    let link = if atom {
        children(item)
            .into_iter()
            .find(|child| tag(child) == Some("link"))
            .and_then(|child| attribute(child, "href"))
            .map_or_else(
                || field(&["link"]),
                |href| DataValue::String(href.to_owned()),
            )
    } else {
        field(&["link"])
    };
    let summary = if atom {
        field(&["summary", "content"])
    } else {
        field(&["description"])
    };
    let published = if atom {
        field(&["published", "updated"])
    } else {
        field(&["pubDate"])
    };
    let id = if atom {
        field(&["id"])
    } else {
        field(&["guid"])
    };
    DataValue::Object(
        [
            ("title".to_owned(), field(&["title"])),
            ("link".to_owned(), link),
            ("summary".to_owned(), summary),
            ("published".to_owned(), published),
            ("id".to_owned(), id),
        ]
        .into_iter()
        .collect(),
    )
}

fn tag(value: &DataValue) -> Option<&str> {
    let DataValue::Object(object) = value else {
        return None;
    };
    match object.get("tag")? {
        DataValue::String(tag) => Some(tag),
        _ => None,
    }
}

fn children(value: &DataValue) -> Vec<&DataValue> {
    let DataValue::Object(object) = value else {
        return Vec::new();
    };
    let Some(DataValue::List(children)) = object.get("children") else {
        return Vec::new();
    };
    children.iter().collect()
}

fn named_children<'a>(value: &'a DataValue, name: &str) -> Vec<&'a DataValue> {
    children(value)
        .into_iter()
        .filter(|child| tag(child) == Some(name))
        .collect()
}

fn attribute<'a>(value: &'a DataValue, name: &str) -> Option<&'a str> {
    let DataValue::Object(object) = value else {
        return None;
    };
    let Some(DataValue::Object(attributes)) = object.get("attributes") else {
        return None;
    };
    match attributes.get(name)? {
        DataValue::String(value) => Some(value),
        _ => None,
    }
}

/// The concatenated non-empty text runs directly inside `value`.
fn text(value: &DataValue) -> String {
    let mut out = String::new();
    for child in children(value) {
        if let DataValue::String(text) = child {
            out.push_str(text);
        }
    }
    out
}

fn child_text(value: &DataValue, name: &str) -> Option<String> {
    let child = children(value)
        .into_iter()
        .find(|child| tag(child) == Some(name))?;
    let text = text(child);
    (!text.trim().is_empty()).then(|| text.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: ParseLimits = ParseLimits {
        max_input_bytes: 64 * 1024,
        max_rows: 2,
        max_columns: 16,
        max_field_bytes: 1024,
        max_depth: 8,
        max_nodes: 200,
    };

    fn field<'a>(item: &'a DataValue, name: &str) -> Option<&'a str> {
        let DataValue::Object(object) = item else {
            return None;
        };
        match object.get(name)? {
            DataValue::String(value) => Some(value),
            _ => None,
        }
    }

    #[test]
    fn rss_items_become_rows() {
        let feed = parse_feed(
            br#"<rss version="2.0"><channel>
                  <title>News</title>
                  <item><title>One</title><link>https://example.com/1</link>
                    <description>First</description><pubDate>Mon, 01 Jan 2024</pubDate>
                    <guid>a</guid></item>
                  <item><title>Two</title><link>https://example.com/2</link></item>
                </channel></rss>"#,
            &LIMITS,
        )
        .unwrap();
        assert_eq!(feed.len(), 2);
        assert_eq!(field(&feed[0], "title"), Some("One"));
        assert_eq!(field(&feed[0], "summary"), Some("First"));
        assert_eq!(field(&feed[0], "published"), Some("Mon, 01 Jan 2024"));
        assert_eq!(field(&feed[0], "id"), Some("a"));
        assert_eq!(field(&feed[1], "summary"), None);
    }

    #[test]
    fn atom_entries_read_the_href_link() {
        let feed = parse_feed(
            br#"<feed xmlns="http://www.w3.org/2005/Atom">
                  <entry><title>One</title><link href="https://example.com/1"/>
                    <summary>First</summary><updated>2024-01-01</updated><id>a</id></entry>
                </feed>"#,
            &LIMITS,
        )
        .unwrap();
        assert_eq!(feed.len(), 1);
        assert_eq!(field(&feed[0], "link"), Some("https://example.com/1"));
        assert_eq!(field(&feed[0], "summary"), Some("First"));
        assert_eq!(field(&feed[0], "published"), Some("2024-01-01"));
        assert_eq!(field(&feed[0], "id"), Some("a"));
    }

    #[test]
    fn non_feeds_and_too_many_items_are_refused() {
        assert_eq!(parse_feed(b"<root/>", &LIMITS), Err(FeedError::NotAFeed));
        let feed = br"<rss><channel><item/><item/><item/></channel></rss>";
        assert_eq!(
            parse_feed(feed, &LIMITS),
            Err(FeedError::TooManyItems { limit: 2 })
        );
    }
}
