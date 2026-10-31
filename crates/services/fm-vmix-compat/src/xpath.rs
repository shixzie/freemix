//! Bounded `XPath` subset evaluated against the vMix-shaped state document.
//!
//! The document is built from [`CompatState`] with the same element names,
//! attribute names, and value formatting as [`CompatState::xml`], so a path
//! that selects something in the rendered XML selects the same value here.

use std::{error::Error, fmt};

use super::{CompatState, uuid, vmix_bool};

/// Maximum `XPath` expression length accepted by [`CompatState::xml_text`].
pub const MAX_XPATH_BYTES: usize = 1_024;
/// Maximum number of location steps in one `XPath` expression.
pub const MAX_XPATH_STEPS: usize = 16;
/// Maximum number of predicates on one location step.
pub const MAX_XPATH_PREDICATES: usize = 4;

/// Errors from evaluating an `XMLTEXT` `XPath` expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XPathError {
    /// The expression was empty.
    Empty,
    /// The expression exceeded [`MAX_XPATH_BYTES`].
    TooLong,
    /// The expression exceeded [`MAX_XPATH_STEPS`] or
    /// [`MAX_XPATH_PREDICATES`].
    TooComplex,
    /// The expression is outside the supported grammar.
    Syntax(&'static str),
    /// The expression is valid but selects no node.
    NotFound,
}

impl fmt::Display for XPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("empty XPath"),
            Self::TooLong => formatter.write_str("XPath exceeds the byte limit"),
            Self::TooComplex => formatter.write_str("XPath exceeds the step or predicate limit"),
            Self::Syntax(reason) => write!(formatter, "unsupported XPath: {reason}"),
            Self::NotFound => formatter.write_str("XPath selected no node"),
        }
    }
}

impl Error for XPathError {}

#[derive(Debug)]
struct Node {
    name: &'static str,
    attributes: Vec<(&'static str, String)>,
    text: String,
    children: Vec<Self>,
}

impl Node {
    fn leaf(name: &'static str, text: impl Into<String>) -> Self {
        Self {
            name,
            attributes: Vec::new(),
            text: text.into(),
            children: Vec::new(),
        }
    }

    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, value)| value.as_str())
    }

    /// `XPath` string-value: own text followed by descendant text in document order.
    fn string_value(&self, output: &mut String) {
        output.push_str(&self.text);
        for child in &self.children {
            child.string_value(output);
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Predicate {
    Position(usize),
    Attribute { name: String, value: String },
    AttributeContains { name: String, value: String },
    AttributeStartsWith { name: String, value: String },
}

#[derive(Debug, Eq, PartialEq)]
struct Step {
    /// `None` is the `*` wildcard.
    name: Option<String>,
    /// `//`: select descendants instead of children.
    descendant: bool,
    predicates: Vec<Predicate>,
}

#[derive(Debug, Eq, PartialEq)]
struct Path {
    steps: Vec<Step>,
    attribute: Option<String>,
}

impl CompatState {
    /// Evaluates the supported `XPath` subset used by vMix `XMLTEXT` and returns
    /// the string value of the first selected node in document order.
    ///
    /// Supported grammar: an optional leading `/`, `/`-separated element name
    /// steps rooted at `vmix` (or `*` for any element), a `//` separator that
    /// selects descendants instead of children, each step with up to
    /// [`MAX_XPATH_PREDICATES`] predicates of the form `[N]` (one-based
    /// position), `[@name='value']` (single or double quotes, exact match),
    /// `[contains(@name,'value')]`, or `[starts-with(@name,'value')]`, an
    /// optional final `text()` step, and an optional final `@name` attribute
    /// step. Element values follow `XPath` string-value semantics, so
    /// an `input` element yields its title followed by its text field values.
    ///
    /// # Errors
    ///
    /// Returns [`XPathError`] for empty, over-limit, unsupported, or
    /// non-matching expressions.
    pub fn xml_text(&self, xpath: &str) -> Result<String, XPathError> {
        let path = parse_path(xpath)?;
        let document = self.document();
        evaluate(&document, &path).ok_or(XPathError::NotFound)
    }

    fn document(&self) -> Node {
        let inputs = self
            .inputs
            .iter()
            .enumerate()
            .map(|(index, input)| Node {
                name: "input",
                attributes: vec![
                    ("key", uuid(input.id)),
                    ("number", (index + 1).to_string()),
                    ("type", input.kind.clone()),
                    ("title", input.title.clone()),
                ],
                text: input.title.clone(),
                children: input
                    .text_fields
                    .iter()
                    .enumerate()
                    .map(|(field_index, field)| Node {
                        name: "text",
                        attributes: vec![
                            ("index", field_index.to_string()),
                            ("name", field.name.clone()),
                        ],
                        text: field.value.clone(),
                        children: Vec::new(),
                    })
                    .collect(),
            })
            .collect();
        let overlays = self
            .overlays
            .iter()
            .enumerate()
            .map(|(index, overlay)| Node {
                name: "overlay",
                attributes: vec![("number", (index + 1).to_string())],
                text: overlay
                    .and_then(|input| self.ordinal(input))
                    .map(|ordinal| ordinal.to_string())
                    .unwrap_or_default(),
                children: Vec::new(),
            })
            .collect();
        let ordinal_text = |id| {
            self.ordinal(id)
                .map(|ordinal| ordinal.to_string())
                .unwrap_or_default()
        };
        Node {
            name: "vmix",
            attributes: Vec::new(),
            text: String::new(),
            children: vec![
                Node::leaf("version", self.version.clone()),
                Node {
                    name: "inputs",
                    attributes: Vec::new(),
                    text: String::new(),
                    children: inputs,
                },
                Node {
                    name: "overlays",
                    attributes: Vec::new(),
                    text: String::new(),
                    children: overlays,
                },
                Node::leaf("preview", ordinal_text(self.preview)),
                Node::leaf("active", ordinal_text(self.active)),
                Node::leaf("recording", vmix_bool(self.recording)),
                Node::leaf("streaming", vmix_bool(self.streaming)),
            ],
        }
    }
}

fn evaluate(document: &Node, path: &Path) -> Option<String> {
    let (root_step, rest) = path.steps.split_first()?;
    let mut context: Vec<&Node> = if root_step.descendant {
        let mut all = Vec::new();
        collect_descendants(document, &mut all);
        select(all, root_step)
    } else {
        select(std::iter::once(document), root_step)
    };
    for step in rest {
        let mut next = Vec::new();
        for node in context {
            if step.descendant {
                let mut all = Vec::new();
                collect_descendants(node, &mut all);
                next.extend(select(all, step));
            } else {
                next.extend(select(node.children.iter(), step));
            }
        }
        context = next;
    }
    match &path.attribute {
        Some(attribute) => context
            .iter()
            .find_map(|node| node.attribute(attribute))
            .map(str::to_owned),
        None => context.first().map(|node| {
            let mut value = String::new();
            node.string_value(&mut value);
            value
        }),
    }
}

fn collect_descendants<'a>(node: &'a Node, output: &mut Vec<&'a Node>) {
    for child in &node.children {
        output.push(child);
        collect_descendants(child, output);
    }
}

fn select<'a>(candidates: impl IntoIterator<Item = &'a Node>, step: &Step) -> Vec<&'a Node> {
    let mut selected: Vec<&Node> = candidates
        .into_iter()
        .filter(|node| step.name.as_deref().is_none_or(|name| node.name == name))
        .collect();
    for predicate in &step.predicates {
        selected = match predicate {
            Predicate::Position(position) => selected
                .get(position - 1)
                .map(|node| vec![*node])
                .unwrap_or_default(),
            Predicate::Attribute { name, value } => selected
                .into_iter()
                .filter(|node| node.attribute(name) == Some(value.as_str()))
                .collect(),
            Predicate::AttributeContains { name, value } => selected
                .into_iter()
                .filter(|node| {
                    node.attribute(name)
                        .is_some_and(|attribute| attribute.contains(value.as_str()))
                })
                .collect(),
            Predicate::AttributeStartsWith { name, value } => selected
                .into_iter()
                .filter(|node| {
                    node.attribute(name)
                        .is_some_and(|attribute| attribute.starts_with(value.as_str()))
                })
                .collect(),
        };
    }
    selected
}

fn parse_path(xpath: &str) -> Result<Path, XPathError> {
    if xpath.len() > MAX_XPATH_BYTES {
        return Err(XPathError::TooLong);
    }
    if xpath.is_empty() {
        return Err(XPathError::Empty);
    }
    let mut parser = Parser {
        bytes: xpath.as_bytes(),
        source: xpath,
        position: 0,
    };
    parser.eat(b'/');
    let mut descendant = parser.eat(b'/');
    let mut steps = Vec::new();
    let mut attribute = None;
    loop {
        if parser.eat(b'@') {
            if descendant {
                return Err(XPathError::Syntax("attribute after '//' is not supported"));
            }
            attribute = Some(parser.name()?.to_owned());
            if !parser.at_end() {
                return Err(XPathError::Syntax("attribute must be the final step"));
            }
            break;
        }
        if steps.len() == MAX_XPATH_STEPS {
            return Err(XPathError::TooComplex);
        }
        if parser.peek_name() == Some("text") {
            let start = parser.position;
            parser.position += 4;
            if parser.eat(b'(') {
                if !parser.eat(b')') {
                    return Err(XPathError::Syntax("expected ')' after text("));
                }
                if steps.is_empty() || !parser.at_end() {
                    return Err(XPathError::Syntax(
                        "text() must be the final step after an element",
                    ));
                }
                // The string value of the selected node is exactly what
                // `xml_text` returns, so `text()` needs no evaluation.
                break;
            }
            parser.position = start;
        }
        let name = if parser.eat(b'*') {
            None
        } else {
            Some(parser.name()?.to_owned())
        };
        let mut predicates = Vec::new();
        while parser.eat(b'[') {
            if predicates.len() == MAX_XPATH_PREDICATES {
                return Err(XPathError::TooComplex);
            }
            predicates.push(parser.predicate()?);
            if !parser.eat(b']') {
                return Err(XPathError::Syntax("expected ']'"));
            }
        }
        steps.push(Step {
            name,
            descendant,
            predicates,
        });
        if parser.at_end() {
            break;
        }
        if !parser.eat(b'/') {
            return Err(XPathError::Syntax("expected '/' between steps"));
        }
        if parser.at_end() {
            return Err(XPathError::Syntax("trailing '/'"));
        }
        descendant = parser.eat(b'/');
    }
    if steps.is_empty() {
        return Err(XPathError::Syntax("expected an element step"));
    }
    Ok(Path { steps, attribute })
}

struct Parser<'a> {
    bytes: &'a [u8],
    source: &'a str,
    position: usize,
}

impl<'a> Parser<'a> {
    const fn at_end(&self) -> bool {
        self.position == self.bytes.len()
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.bytes.get(self.position) == Some(&byte) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn peek_name(&self) -> Option<&'a str> {
        let mut position = self.position;
        while self
            .bytes
            .get(position)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            position += 1;
        }
        if position == self.position
            || !self.bytes[self.position].is_ascii_alphabetic() && self.bytes[self.position] != b'_'
        {
            return None;
        }
        Some(&self.source[self.position..position])
    }

    fn literal(&mut self) -> Result<String, XPathError> {
        let quote = match self.bytes.get(self.position) {
            Some(quote @ (b'\'' | b'"')) => *quote,
            _ => return Err(XPathError::Syntax("expected a quoted literal")),
        };
        self.position += 1;
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| *byte != quote)
        {
            self.position += 1;
        }
        if self.at_end() {
            return Err(XPathError::Syntax("unterminated literal"));
        }
        let value = self.source[start..self.position].to_owned();
        self.position += 1;
        Ok(value)
    }

    fn name(&mut self) -> Result<&'a str, XPathError> {
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            self.position += 1;
        }
        let name = &self.source[start..self.position];
        if name.is_empty()
            || !name.as_bytes()[0].is_ascii_alphabetic() && name.as_bytes()[0] != b'_'
        {
            return Err(XPathError::Syntax("expected a name"));
        }
        Ok(name)
    }

    fn predicate(&mut self) -> Result<Predicate, XPathError> {
        if self.eat(b'@') {
            let name = self.name()?.to_owned();
            if !self.eat(b'=') {
                return Err(XPathError::Syntax("expected '=' in attribute predicate"));
            }
            let value = self.literal()?;
            return Ok(Predicate::Attribute { name, value });
        }
        if let Some(name) = self.peek_name() {
            let kind = match name {
                "contains" => Some(true),
                "starts-with" => Some(false),
                _ => None,
            };
            if let Some(contains) = kind {
                self.position += name.len();
                if !self.eat(b'(') {
                    return Err(XPathError::Syntax("expected '(' after a function"));
                }
                if !self.eat(b'@') {
                    return Err(XPathError::Syntax(
                        "expected an attribute in a function predicate",
                    ));
                }
                let attribute = self.name()?.to_owned();
                if !self.eat(b',') {
                    return Err(XPathError::Syntax("expected ',' in a function predicate"));
                }
                let value = self.literal()?;
                if !self.eat(b')') {
                    return Err(XPathError::Syntax(
                        "expected ')' after a function predicate",
                    ));
                }
                return Ok(if contains {
                    Predicate::AttributeContains {
                        name: attribute,
                        value,
                    }
                } else {
                    Predicate::AttributeStartsWith {
                        name: attribute,
                        value,
                    }
                });
            }
        }
        let start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(u8::is_ascii_digit)
        {
            self.position += 1;
        }
        self.source[start..self.position]
            .parse::<usize>()
            .ok()
            .filter(|position| *position >= 1)
            .map(Predicate::Position)
            .ok_or(XPathError::Syntax(
                "expected a one-based position or attribute predicate",
            ))
    }
}
