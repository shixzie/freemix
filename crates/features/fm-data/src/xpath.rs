//! A bounded `XPath` subset over parsed XML documents (GX-004).
//!
//! [`crate::parse_xml`] turns one document into a [`DataValue::Object`] tree.
//! This module evaluates a small, strictly bounded `XPath` expression against
//! that tree so a data source can address arbitrary XML, not only the RSS,
//! Atom, and HTML-table shapes.
//!
//! Supported expression forms:
//!
//! - an absolute path (`/rss/channel/item`), whose first step is the root
//!   element,
//! - a path relative to the root element (`channel/item`; a first step that
//!   names the root element also matches the root, so `rss/channel` works),
//! - a descendant step (`//item`, `rss//item`),
//! - `*` (every element), `@name` and `@*` (attributes), and `text()` (all
//!   text inside an element, including inline markup),
//! - one predicate per element step: `[n]` (a one-based index), `[@name]`
//!   (the attribute exists), or `[@name='value']` (`"` quotes also work).
//!
//! Anything else — axes, functions, unions, nested or boolean predicates,
//! namespace resolution — is refused rather than approximated. A child step's
//! numeric predicate counts per parent; a descendant step's counts across the
//! whole step. A well-formed path that matches nothing returns an empty list.
//!
//! The expression, its steps, its index, and its result are all bounded, so
//! an expression cannot make evaluation grow past [`MAX_XPATH_MATCHES`].

use core::fmt;

use crate::value::DataValue;

/// Longest accepted expression, in bytes.
pub const MAX_XPATH_BYTES: usize = 512;
/// Most steps one expression may hold.
pub const MAX_XPATH_STEPS: usize = 32;
/// Largest one-based predicate index.
pub const MAX_XPATH_INDEX: u32 = 10_000;
/// Most nodes one step may hold.
pub const MAX_XPATH_MATCHES: usize = 4_096;

/// Why an `XPath` expression was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XPathError {
    /// The expression is empty or ends in a separator.
    Empty,
    /// The expression is longer than [`MAX_XPATH_BYTES`].
    TooLong,
    /// The expression has more than [`MAX_XPATH_STEPS`] steps.
    TooManySteps,
    /// A step is not a name, `*`, `@name`, `@*`, or `text()`, or an attribute
    /// or text step begins the path.
    InvalidStep,
    /// A predicate is not `[n]`, `[@name]`, or `[@name='value']`.
    InvalidPredicate,
    /// The expression selects more than [`MAX_XPATH_MATCHES`] nodes.
    TooManyMatches,
}

impl fmt::Display for XPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("XPath expression is empty"),
            Self::TooLong => write!(
                formatter,
                "XPath expression is longer than {MAX_XPATH_BYTES} bytes"
            ),
            Self::TooManySteps => write!(
                formatter,
                "XPath expression has more than {MAX_XPATH_STEPS} steps"
            ),
            Self::InvalidStep => formatter.write_str("XPath step is not supported"),
            Self::InvalidPredicate => formatter.write_str("XPath predicate is not supported"),
            Self::TooManyMatches => write!(
                formatter,
                "XPath expression selects more than {MAX_XPATH_MATCHES} nodes"
            ),
        }
    }
}

impl std::error::Error for XPathError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Axis {
    Child,
    Descendant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Test {
    Name(String),
    Wildcard,
    /// `@name`, or every attribute for `@*`.
    Attribute(Option<String>),
    Text,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Predicate {
    Index(usize),
    HasAttribute(String),
    AttributeEquals(String, String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Step {
    axis: Axis,
    test: Test,
    predicate: Option<Predicate>,
}

/// A step result: a node borrowed from the document, or text built by a
/// `text()` step.
enum Node<'a> {
    Borrowed(&'a DataValue),
    Text(String),
}

/// Evaluates a bounded `XPath` expression against a parsed XML document.
///
/// The returned values are clones of the selected nodes: an element keeps its
/// [`DataValue::Object`] shape, and an attribute or `text()` selects a
/// [`DataValue::String`].
///
/// # Errors
///
/// Returns [`XPathError`] for an unsupported expression form or a violated
/// bound. A well-formed path that matches nothing is `Ok(vec![])`.
pub fn select_xml(document: &DataValue, expression: &str) -> Result<Vec<DataValue>, XPathError> {
    let (absolute, steps) = parse(expression)?;
    let mut current: Vec<Node<'_>> = vec![Node::Borrowed(document)];
    for (index, step) in steps.iter().enumerate() {
        let first = index == 0;
        let mut matches: Vec<Node<'_>> = Vec::new();
        match step.axis {
            Axis::Child => {
                for node in &current {
                    let Node::Borrowed(node) = node else {
                        continue;
                    };
                    let mut selected: Vec<Node<'_>> = Vec::new();
                    match (&step.test, first, absolute) {
                        // `/name` and `/*` match the root element itself.
                        (Test::Name(name), true, true) => {
                            if element_tag(node) == Some(name.as_str()) {
                                selected.push(Node::Borrowed(node));
                            }
                        }
                        (Test::Wildcard, true, true) => {
                            if element_tag(node).is_some() {
                                selected.push(Node::Borrowed(node));
                            }
                        }
                        // A relative first step matches the root's children,
                        // and a name that is the root's tag matches the root.
                        (Test::Name(name), true, false) => {
                            if element_tag(node) == Some(name.as_str()) {
                                selected.push(Node::Borrowed(node));
                            }
                            selected.extend(
                                child_elements(node)
                                    .filter(|child| element_tag(child) == Some(name.as_str()))
                                    .map(Node::Borrowed),
                            );
                        }
                        (Test::Wildcard, _, _) => {
                            selected.extend(child_elements(node).map(Node::Borrowed));
                        }
                        (Test::Name(name), _, _) => {
                            selected.extend(
                                child_elements(node)
                                    .filter(|child| element_tag(child) == Some(name.as_str()))
                                    .map(Node::Borrowed),
                            );
                        }
                        (Test::Attribute(name), _, _) => {
                            let Some(attributes) = element_attribute_map(node) else {
                                continue;
                            };
                            selected.extend(
                                attributes
                                    .iter()
                                    .filter(|(key, _)| {
                                        name.as_ref().is_none_or(|name| name == *key)
                                    })
                                    .map(|(_, value)| Node::Borrowed(value)),
                            );
                        }
                        (Test::Text, _, _) => {
                            if element_tag(node).is_some() {
                                selected.push(Node::Text(element_text(node)));
                            }
                        }
                    }
                    apply_predicate(&mut selected, step.predicate.as_ref());
                    matches.extend(selected);
                    if matches.len() > MAX_XPATH_MATCHES {
                        return Err(XPathError::TooManyMatches);
                    }
                }
            }
            Axis::Descendant => {
                for node in &current {
                    let Node::Borrowed(node) = node else {
                        continue;
                    };
                    // The leading `//` includes the root; a later `//` only
                    // selects descendants of its context.
                    if first && element_tag(node).is_some() && matches_test(node, &step.test) {
                        matches.push(Node::Borrowed(node));
                    }
                    collect_descendants(node, &step.test, &mut matches);
                    if matches.len() > MAX_XPATH_MATCHES {
                        return Err(XPathError::TooManyMatches);
                    }
                }
                apply_predicate(&mut matches, step.predicate.as_ref());
            }
        }
        current = matches;
        if current.is_empty() {
            break;
        }
    }
    Ok(current
        .into_iter()
        .map(|node| match node {
            Node::Borrowed(node) => node.clone(),
            Node::Text(text) => DataValue::String(text),
        })
        .collect())
}

fn parse(expression: &str) -> Result<(bool, Vec<Step>), XPathError> {
    if expression.is_empty() {
        return Err(XPathError::Empty);
    }
    if expression.len() > MAX_XPATH_BYTES {
        return Err(XPathError::TooLong);
    }
    if expression.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(XPathError::InvalidStep);
    }
    let (absolute, mut rest, mut axis) = if let Some(rest) = expression.strip_prefix("//") {
        (false, rest, Axis::Descendant)
    } else if let Some(rest) = expression.strip_prefix('/') {
        (true, rest, Axis::Child)
    } else {
        (false, expression, Axis::Child)
    };
    let mut steps = Vec::new();
    loop {
        let end = rest.find('/').unwrap_or(rest.len());
        let (segment, tail) = rest.split_at(end);
        if segment.is_empty() {
            return Err(XPathError::Empty);
        }
        let test = parse_test(segment)?;
        if matches!(test, Test::Attribute(_) | Test::Text) && steps.is_empty() {
            // A path starts at an element; `@id`, `text()`, and their
            // descendant forms cannot begin one.
            return Err(XPathError::InvalidStep);
        }
        if matches!(test, Test::Attribute(_) | Test::Text) && axis == Axis::Descendant {
            // Descendant steps select elements; attributes and text are
            // selected by child steps (`//item/@id`).
            return Err(XPathError::InvalidStep);
        }
        steps.push(Step {
            axis,
            test,
            predicate: parse_predicate(segment)?,
        });
        if steps.len() > MAX_XPATH_STEPS {
            return Err(XPathError::TooManySteps);
        }
        if tail.is_empty() {
            break;
        }
        if let Some(next) = tail.strip_prefix("//") {
            axis = Axis::Descendant;
            rest = next;
        } else if let Some(next) = tail.strip_prefix('/') {
            axis = Axis::Child;
            rest = next;
        } else {
            return Err(XPathError::InvalidStep);
        }
        if rest.is_empty() {
            return Err(XPathError::Empty);
        }
    }
    Ok((absolute, steps))
}

/// The step text before its predicate.
fn step_head(segment: &str) -> &str {
    match segment.find('[') {
        Some(index) => &segment[..index],
        None => segment,
    }
}

fn parse_test(segment: &str) -> Result<Test, XPathError> {
    let head = step_head(segment);
    if head == "text()" {
        return Ok(Test::Text);
    }
    if let Some(rest) = head.strip_prefix('@') {
        if rest == "*" {
            return Ok(Test::Attribute(None));
        }
        return valid_name(rest)
            .then(|| Test::Attribute(Some(rest.to_owned())))
            .ok_or(XPathError::InvalidStep);
    }
    if head == "*" {
        return Ok(Test::Wildcard);
    }
    valid_name(head)
        .then(|| Test::Name(head.to_owned()))
        .ok_or(XPathError::InvalidStep)
}

fn parse_predicate(segment: &str) -> Result<Option<Predicate>, XPathError> {
    let Some(start) = segment.find('[') else {
        return Ok(None);
    };
    let Some(body) = segment[start + 1..].strip_suffix(']') else {
        return Err(XPathError::InvalidPredicate);
    };
    if body.contains('[') || body.contains(']') {
        return Err(XPathError::InvalidPredicate);
    }
    if let Some(attribute) = body.strip_prefix('@') {
        let (name, value) = match attribute.split_once('=') {
            Some((name, value)) => {
                let value = value
                    .strip_prefix('\'')
                    .and_then(|rest| rest.strip_suffix('\''))
                    .or_else(|| {
                        value
                            .strip_prefix('"')
                            .and_then(|rest| rest.strip_suffix('"'))
                    })
                    .ok_or(XPathError::InvalidPredicate)?;
                (name, Some(value))
            }
            None => (attribute, None),
        };
        if !valid_name(name) {
            return Err(XPathError::InvalidPredicate);
        }
        return Ok(Some(match value {
            Some(value) => Predicate::AttributeEquals(name.to_owned(), value.to_owned()),
            None => Predicate::HasAttribute(name.to_owned()),
        }));
    }
    let index = body
        .parse::<u32>()
        .ok()
        .filter(|index| (1..=MAX_XPATH_INDEX).contains(index))
        .ok_or(XPathError::InvalidPredicate)?;
    Ok(Some(Predicate::Index(usize::try_from(index).unwrap_or(1))))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains("::")
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':')
        })
}

fn matches_test(node: &DataValue, test: &Test) -> bool {
    match test {
        Test::Name(name) => element_tag(node) == Some(name.as_str()),
        Test::Wildcard => element_tag(node).is_some(),
        Test::Attribute(_) | Test::Text => false,
    }
}

fn apply_predicate(nodes: &mut Vec<Node<'_>>, predicate: Option<&Predicate>) {
    match predicate {
        None => {}
        Some(Predicate::Index(index)) => {
            if let Some(kept) = nodes.get(*index - 1) {
                let kept = match kept {
                    Node::Borrowed(node) => Node::Borrowed(node),
                    Node::Text(text) => Node::Text(text.clone()),
                };
                nodes.clear();
                nodes.push(kept);
            } else {
                nodes.clear();
            }
        }
        Some(Predicate::HasAttribute(name)) => {
            nodes.retain(|node| match node {
                Node::Borrowed(node) => attribute(node, name).is_some(),
                Node::Text(_) => false,
            });
        }
        Some(Predicate::AttributeEquals(name, value)) => {
            nodes.retain(|node| match node {
                Node::Borrowed(node) => attribute(node, name) == Some(value.as_str()),
                Node::Text(_) => false,
            });
        }
    }
}

fn collect_descendants<'a>(node: &'a DataValue, test: &Test, out: &mut Vec<Node<'a>>) {
    for child in child_elements(node) {
        if matches_test(child, test) {
            out.push(Node::Borrowed(child));
            if out.len() > MAX_XPATH_MATCHES {
                return;
            }
        }
        collect_descendants(child, test, out);
    }
}

fn fields(node: &DataValue) -> Option<&std::collections::BTreeMap<String, DataValue>> {
    match node {
        DataValue::Object(object) => Some(object),
        _ => None,
    }
}

fn element_tag(node: &DataValue) -> Option<&str> {
    match fields(node)?.get("tag")? {
        DataValue::String(tag) => Some(tag),
        _ => None,
    }
}

fn element_attribute_map(
    node: &DataValue,
) -> Option<&std::collections::BTreeMap<String, DataValue>> {
    match fields(node)?.get("attributes")? {
        DataValue::Object(attributes) => Some(attributes),
        _ => None,
    }
}

fn attribute<'a>(node: &'a DataValue, name: &str) -> Option<&'a str> {
    match element_attribute_map(node)?.get(name)? {
        DataValue::String(value) => Some(value),
        _ => None,
    }
}

fn child_elements(node: &DataValue) -> impl Iterator<Item = &DataValue> {
    let children = match fields(node).and_then(|fields| fields.get("children")) {
        Some(DataValue::List(children)) => children.as_slice(),
        _ => &[],
    };
    children.iter().filter(|child| element_tag(child).is_some())
}

/// The text inside `node`: every text run in document order, including runs
/// nested in inline markup.
fn element_text(node: &DataValue) -> String {
    let mut text = String::new();
    collect_text(node, &mut text);
    text
}

fn collect_text(node: &DataValue, out: &mut String) {
    let Some(fields) = fields(node) else {
        return;
    };
    if let Some(DataValue::List(children)) = fields.get("children") {
        for child in children {
            match child {
                DataValue::String(run) => out.push_str(run),
                other => collect_text(other, out),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::ParseLimits;
    use crate::xml::parse_xml;

    fn document() -> DataValue {
        parse_xml(
            br#"<rss version="2.0">
                  <channel>
                    <title>News</title>
                    <item id="a"><title>First</title><link>https://a.example/</link></item>
                    <item id="b"><title>Second <b>bold</b></title><link>https://b.example/</link></item>
                  </channel>
                </rss>"#,
            &ParseLimits::default(),
        )
        .unwrap()
    }

    fn strings(values: Vec<DataValue>) -> Vec<String> {
        values
            .into_iter()
            .map(|value| match value {
                DataValue::String(text) => text,
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn absolute_and_relative_paths_select_elements() {
        let document = document();
        assert_eq!(select_xml(&document, "/rss/channel/item").unwrap().len(), 2);
        assert_eq!(select_xml(&document, "rss/channel/item").unwrap().len(), 2);
        assert_eq!(
            select_xml(&document, "channel/title").unwrap().len(),
            1,
            "a relative first step matches the root's children"
        );
        assert!(select_xml(&document, "/rss/missing").unwrap().is_empty());
        assert!(select_xml(&document, "//nothing").unwrap().is_empty());
    }

    #[test]
    fn descendant_wildcards_attributes_and_text_select_values() {
        let document = document();
        assert_eq!(select_xml(&document, "//item").unwrap().len(), 2);
        assert_eq!(select_xml(&document, "/rss/channel/*").unwrap().len(), 3);
        assert_eq!(
            strings(select_xml(&document, "//item/@id").unwrap()),
            ["a", "b"]
        );
        assert_eq!(
            strings(select_xml(&document, "//item/title/text()").unwrap()),
            ["First", "Second bold"]
        );
        assert_eq!(
            strings(select_xml(&document, "/rss/@version").unwrap()),
            ["2.0"]
        );
        assert_eq!(
            strings(select_xml(&document, "//item[1]/@*").unwrap()),
            ["a"]
        );
    }

    #[test]
    fn predicates_select_by_index_and_attribute() {
        let document = document();
        assert_eq!(
            strings(select_xml(&document, "//item[2]/title/text()").unwrap()),
            ["Second bold"]
        );
        assert_eq!(
            strings(select_xml(&document, "//item[@id=\"b\"]/title/text()").unwrap()),
            ["Second bold"]
        );
        assert_eq!(
            strings(select_xml(&document, "//item[@id='a']/title/text()").unwrap()),
            ["First"]
        );
        assert_eq!(select_xml(&document, "//item[@id]/@id").unwrap().len(), 2);
        assert!(select_xml(&document, "//item[3]").unwrap().is_empty());
        assert!(select_xml(&document, "//item[@id='c']").unwrap().is_empty());
    }

    #[test]
    fn unsupported_expressions_are_refused() {
        let document = document();
        for expression in [
            "",
            "/",
            "//",
            "/rss/",
            "//item/",
            "//item[",
            "//item[]",
            "//item[0]",
            "//item[10001]",
            "//item[last()]",
            "//item[contains(title,'a')]",
            "//item[@id='a'][2]",
            "//item[title]",
            "child::item",
            "//item | //channel",
            "//item[@id=a]",
            "//item[@id='a]",
            "//it em",
            "//item//",
            "@id",
            "text()",
        ] {
            assert!(
                select_xml(&document, expression).is_err(),
                "accepted {expression:?}"
            );
        }
        assert_eq!(
            select_xml(&document, &"a/".repeat(MAX_XPATH_STEPS + 2)),
            Err(XPathError::TooManySteps)
        );
        assert_eq!(
            select_xml(&document, &"a".repeat(MAX_XPATH_BYTES + 1)),
            Err(XPathError::TooLong)
        );
    }
}
