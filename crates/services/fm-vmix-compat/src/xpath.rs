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
/// Maximum number of union operands in one `XPath` expression.
pub const MAX_XPATH_PATHS: usize = 4;
/// Maximum nesting depth of a numeric predicate expression.
pub const MAX_XPATH_EXPRESSION_DEPTH: usize = 8;

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
    Attribute {
        name: String,
        value: String,
    },
    AttributeContains {
        name: String,
        value: String,
    },
    AttributeStartsWith {
        name: String,
        value: String,
    },
    /// `[EXPR <op> EXPR]`: a numeric comparison of two bounded
    /// expressions.
    Numeric {
        left: NumExpr,
        comparison: Comparison,
        right: NumExpr,
    },
}

/// A bounded integer expression in a numeric predicate.
#[derive(Clone, Debug, Eq, PartialEq)]
enum NumExpr {
    Integer(i64),
    /// An attribute parsed as an integer.
    Attribute(String),
    /// The context node's one-based position in the current node-set.
    Position,
    /// The size of the current node-set.
    Last,
    Add(Box<Self>, Box<Self>),
    Subtract(Box<Self>, Box<Self>),
    Multiply(Box<Self>, Box<Self>),
    Divide(Box<Self>, Box<Self>),
    Modulo(Box<Self>, Box<Self>),
}

impl NumExpr {
    fn evaluate(&self, node: &Node, position: i64, last: i64) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            Self::Attribute(name) => node.attribute(name)?.parse().ok(),
            Self::Position => Some(position),
            Self::Last => Some(last),
            Self::Add(left, right) => left
                .evaluate(node, position, last)?
                .checked_add(right.evaluate(node, position, last)?),
            Self::Subtract(left, right) => left
                .evaluate(node, position, last)?
                .checked_sub(right.evaluate(node, position, last)?),
            Self::Multiply(left, right) => left
                .evaluate(node, position, last)?
                .checked_mul(right.evaluate(node, position, last)?),
            Self::Divide(left, right) => left
                .evaluate(node, position, last)?
                .checked_div(right.evaluate(node, position, last)?),
            Self::Modulo(left, right) => left
                .evaluate(node, position, last)?
                .checked_rem(right.evaluate(node, position, last)?),
        }
    }
}

/// A numeric comparison operator in a predicate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Comparison {
    Eq,
    NotEq,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

impl Comparison {
    const fn holds(self, left: i64, right: i64) -> bool {
        match self {
            Self::Eq => left == right,
            Self::NotEq => left != right,
            Self::Less => left < right,
            Self::LessOrEqual => left <= right,
            Self::Greater => left > right,
            Self::GreaterOrEqual => left >= right,
        }
    }
}

/// A location step's axis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Axis {
    /// `/` (and the implicit leading step): children.
    Child,
    /// `//`: descendants.
    Descendant,
    /// `self::`: the context node.
    SelfAxis,
    /// `parent::`: the context node's parent.
    Parent,
    /// `ancestor::`: every ancestor, nearest first.
    Ancestor,
    /// `following-sibling::`: later siblings, in document order.
    FollowingSibling,
    /// `preceding-sibling::`: earlier siblings, in document order.
    PrecedingSibling,
}

#[derive(Debug, Eq, PartialEq)]
struct Step {
    /// `None` is the `*` wildcard.
    name: Option<String>,
    axis: Axis,
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
    /// optional final `text()` step, an optional final `@name` attribute
    /// step, and up to [`MAX_XPATH_PATHS`] location paths joined by `|`
    /// (a union, whose first node in document order is used).
    /// Element values follow `XPath` string-value semantics, so
    /// an `input` element yields its title followed by its text field values.
    ///
    /// # Errors
    ///
    /// Returns [`XPathError`] for empty, over-limit, unsupported, or
    /// non-matching expressions.
    pub fn xml_text(&self, xpath: &str) -> Result<String, XPathError> {
        let paths = parse_expression(xpath)?;
        let document = self.document();
        let order = document_order(&document);
        let parents = parent_map(&document);
        let mut best: Option<(usize, usize, &Node, Option<&str>)> = None;
        for (index, path) in paths.iter().enumerate() {
            let Some((node, attribute)) = select_path(&document, path, &parents) else {
                continue;
            };
            let position = order
                .get(&(node as *const Node))
                .copied()
                .unwrap_or(usize::MAX);
            if best
                .as_ref()
                .is_none_or(|(best_position, best_index, _, _)| {
                    (position, index) < (*best_position, *best_index)
                })
            {
                best = Some((position, index, node, attribute));
            }
        }
        let (_, _, node, attribute) = best.ok_or(XPathError::NotFound)?;
        match attribute {
            Some(value) => Ok(value.to_owned()),
            None => {
                let mut value = String::new();
                node.string_value(&mut value);
                Ok(value)
            }
        }
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

fn select_path<'a>(
    document: &'a Node,
    path: &Path,
    parents: &std::collections::HashMap<*const Node, &'a Node>,
) -> Option<(&'a Node, Option<&'a str>)> {
    let (root_step, rest) = path.steps.split_first()?;
    let mut context = match root_step.axis {
        // The path root is the document itself; a leading child step
        // matches it as the old evaluator did.
        Axis::Child | Axis::SelfAxis => select(std::iter::once(document), root_step),
        Axis::Descendant => {
            let mut all = Vec::new();
            collect_descendants(document, &mut all);
            select(all, root_step)
        }
        _ => Vec::new(),
    };
    for step in rest {
        let mut candidates = Vec::new();
        for node in context {
            candidates.extend(step_candidates(node, step, parents));
        }
        context = select(candidates, step);
    }
    match &path.attribute {
        Some(attribute) => context
            .iter()
            .find_map(|node| node.attribute(attribute).map(|value| (*node, Some(value)))),
        None => context.first().map(|node| (*node, None)),
    }
}

/// The nodes one step selects from `node` along its axis.
fn step_candidates<'a>(
    node: &'a Node,
    step: &Step,
    parents: &std::collections::HashMap<*const Node, &'a Node>,
) -> Vec<&'a Node> {
    match step.axis {
        Axis::Child => node.children.iter().collect(),
        Axis::Descendant => {
            let mut all = Vec::new();
            collect_descendants(node, &mut all);
            all
        }
        Axis::SelfAxis => vec![node],
        Axis::Parent => parents
            .get(&(node as *const Node))
            .copied()
            .into_iter()
            .collect(),
        Axis::Ancestor => {
            let mut ancestors = Vec::new();
            let mut current = node;
            while let Some(parent) = parents.get(&(current as *const Node)).copied() {
                ancestors.push(parent);
                current = parent;
            }
            ancestors
        }
        Axis::FollowingSibling => sibling_candidates(node, parents, true),
        Axis::PrecedingSibling => sibling_candidates(node, parents, false),
    }
}

fn sibling_candidates<'a>(
    node: &'a Node,
    parents: &std::collections::HashMap<*const Node, &'a Node>,
    following: bool,
) -> Vec<&'a Node> {
    let Some(parent) = parents.get(&(node as *const Node)).copied() else {
        return Vec::new();
    };
    let Some(index) = parent
        .children
        .iter()
        .position(|child| std::ptr::eq(child, node))
    else {
        return Vec::new();
    };
    if following {
        parent.children[index + 1..].iter().collect()
    } else {
        parent.children[..index].iter().collect()
    }
}

fn parent_map(document: &Node) -> std::collections::HashMap<*const Node, &Node> {
    fn visit<'a>(node: &'a Node, map: &mut std::collections::HashMap<*const Node, &'a Node>) {
        for child in &node.children {
            map.insert(child as *const Node, node);
            visit(child, map);
        }
    }
    let mut map = std::collections::HashMap::new();
    visit(document, &mut map);
    map
}

/// Every node's document-order index, for choosing a union's first node.
fn document_order(document: &Node) -> std::collections::HashMap<*const Node, usize> {
    fn visit(
        node: &Node,
        next: &mut usize,
        order: &mut std::collections::HashMap<*const Node, usize>,
    ) {
        order.insert(node as *const Node, *next);
        *next += 1;
        for child in &node.children {
            visit(child, next, order);
        }
    }
    let mut order = std::collections::HashMap::new();
    let mut next = 0;
    visit(document, &mut next, &mut order);
    order
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
    let last = i64::try_from(selected.len()).unwrap_or(i64::MAX);
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
            Predicate::Numeric {
                left,
                comparison,
                right,
            } => selected
                .into_iter()
                .enumerate()
                .filter(|(index, node)| {
                    let position = i64::try_from(*index).unwrap_or(i64::MAX).saturating_add(1);
                    left.evaluate(node, position, last)
                        .zip(right.evaluate(node, position, last))
                        .is_some_and(|(left, right)| comparison.holds(left, right))
                })
                .map(|(_, node)| node)
                .collect(),
        };
    }
    selected
}

/// Splits an expression into its union operands and parses each one.
/// `|` inside a quoted literal or a predicate/bracket does not split.
fn parse_expression(xpath: &str) -> Result<Vec<Path>, XPathError> {
    if xpath.len() > MAX_XPATH_BYTES {
        return Err(XPathError::TooLong);
    }
    if xpath.is_empty() {
        return Err(XPathError::Empty);
    }
    let mut segments = Vec::new();
    let mut start = 0;
    let mut quote: Option<u8> = None;
    let mut depth = 0_usize;
    for (index, byte) in xpath.bytes().enumerate() {
        match quote {
            Some(open) => {
                if byte == open {
                    quote = None;
                }
            }
            None => match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'[' | b'(' => depth += 1,
                b']' | b')' => depth = depth.saturating_sub(1),
                b'|' if depth == 0 => {
                    segments.push(&xpath[start..index]);
                    start = index + 1;
                }
                _ => {}
            },
        }
    }
    if quote.is_some() {
        return Err(XPathError::Syntax("unterminated literal"));
    }
    if depth != 0 {
        return Err(XPathError::Syntax("unbalanced bracket"));
    }
    segments.push(&xpath[start..]);
    if segments.len() > MAX_XPATH_PATHS {
        return Err(XPathError::TooComplex);
    }
    segments
        .into_iter()
        .map(|segment| {
            let segment = segment.trim();
            if segment.is_empty() {
                return Err(XPathError::Syntax("empty union operand"));
            }
            parse_path(segment)
        })
        .collect()
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
    let mut axis = if parser.eat(b'/') {
        Axis::Descendant
    } else {
        Axis::Child
    };
    let mut steps = Vec::new();
    let mut attribute = None;
    loop {
        if parser.eat(b'@') {
            if axis == Axis::Descendant {
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
        if let Some(explicit) = parser.axis() {
            if axis == Axis::Descendant {
                return Err(XPathError::Syntax(
                    "an explicit axis after '//' is not supported",
                ));
            }
            axis = explicit;
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
            axis,
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
        axis = if parser.eat(b'/') {
            Axis::Descendant
        } else {
            Axis::Child
        };
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

    /// Eats an explicit `axis::` prefix, if one of the supported axes
    /// starts here.
    fn axis(&mut self) -> Option<Axis> {
        let name = self.peek_name()?;
        let axis = match name {
            "child" => Axis::Child,
            "descendant" => Axis::Descendant,
            "self" => Axis::SelfAxis,
            "parent" => Axis::Parent,
            "ancestor" => Axis::Ancestor,
            "following-sibling" => Axis::FollowingSibling,
            "preceding-sibling" => Axis::PrecedingSibling,
            _ => return None,
        };
        let after = self.position + name.len();
        if self.source[after..].starts_with("::") {
            self.position = after + 2;
            Some(axis)
        } else {
            None
        }
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

    fn comparison(&mut self) -> Result<Comparison, XPathError> {
        if self.eat(b'=') {
            return Ok(Comparison::Eq);
        }
        if self.eat(b'!') {
            if self.eat(b'=') {
                return Ok(Comparison::NotEq);
            }
            return Err(XPathError::Syntax("expected '!=' in a predicate"));
        }
        if self.eat(b'<') {
            return Ok(if self.eat(b'=') {
                Comparison::LessOrEqual
            } else {
                Comparison::Less
            });
        }
        if self.eat(b'>') {
            return Ok(if self.eat(b'=') {
                Comparison::GreaterOrEqual
            } else {
                Comparison::Greater
            });
        }
        Err(XPathError::Syntax("expected a comparison operator"))
    }

    fn integer(&mut self) -> Result<i64, XPathError> {
        let start = self.position;
        let negative = self.eat(b'-');
        let digits_start = self.position;
        while self
            .bytes
            .get(self.position)
            .is_some_and(u8::is_ascii_digit)
        {
            self.position += 1;
        }
        let digits = &self.source[digits_start..self.position];
        if digits.is_empty()
            || (digits.len() > 1 && digits.starts_with('0'))
            || (negative && digits == "0")
        {
            return Err(XPathError::Syntax("expected a canonical integer"));
        }
        self.source[start..self.position]
            .parse::<i64>()
            .map_err(|_| XPathError::Syntax("integer is out of range"))
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

    fn skip_whitespace(&mut self) {
        while self
            .bytes
            .get(self.position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.position += 1;
        }
    }

    fn string_equality_ahead(&self) -> bool {
        let mut position = self.position;
        while self
            .bytes
            .get(position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            position += 1;
        }
        if self.bytes.get(position) != Some(&b'=') {
            return false;
        }
        position += 1;
        while self
            .bytes
            .get(position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            position += 1;
        }
        matches!(self.bytes.get(position), Some(b'\'' | b'"'))
    }

    fn eat_word(&mut self, word: &str) -> bool {
        let bytes = word.as_bytes();
        if self.bytes.get(self.position..self.position + bytes.len()) != Some(bytes) {
            return false;
        }
        let end = self.position + bytes.len();
        if self
            .bytes
            .get(end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return false;
        }
        self.position = end;
        true
    }

    fn numeric_expression(&mut self) -> Result<NumExpr, XPathError> {
        self.numeric_sum(0)
    }

    fn numeric_sum(&mut self, depth: usize) -> Result<NumExpr, XPathError> {
        let mut left = self.numeric_product(depth)?;
        loop {
            self.skip_whitespace();
            if self.eat(b'+') {
                let right = self.numeric_product(depth)?;
                left = NumExpr::Add(Box::new(left), Box::new(right));
            } else if self.eat(b'-') {
                let right = self.numeric_product(depth)?;
                left = NumExpr::Subtract(Box::new(left), Box::new(right));
            } else {
                return Ok(left);
            }
        }
    }

    fn numeric_product(&mut self, depth: usize) -> Result<NumExpr, XPathError> {
        let mut left = self.numeric_primary(depth)?;
        loop {
            self.skip_whitespace();
            if self.eat(b'*') {
                let right = self.numeric_primary(depth)?;
                left = NumExpr::Multiply(Box::new(left), Box::new(right));
            } else if self.eat_word("div") {
                let right = self.numeric_primary(depth)?;
                left = NumExpr::Divide(Box::new(left), Box::new(right));
            } else if self.eat_word("mod") {
                let right = self.numeric_primary(depth)?;
                left = NumExpr::Modulo(Box::new(left), Box::new(right));
            } else {
                return Ok(left);
            }
        }
    }

    fn numeric_primary(&mut self, depth: usize) -> Result<NumExpr, XPathError> {
        if depth >= MAX_XPATH_EXPRESSION_DEPTH {
            return Err(XPathError::TooComplex);
        }
        self.skip_whitespace();
        if self.eat(b'(') {
            let expression = self.numeric_sum(depth + 1)?;
            self.skip_whitespace();
            if !self.eat(b')') {
                return Err(XPathError::Syntax("expected ')'"));
            }
            return Ok(expression);
        }
        if self.eat(b'@') {
            return Ok(NumExpr::Attribute(self.name()?.to_owned()));
        }
        if self.peek_name() == Some("position") {
            self.position += "position".len();
            if !self.eat(b'(') {
                return Err(XPathError::Syntax("expected '(' after position"));
            }
            if !self.eat(b')') {
                return Err(XPathError::Syntax("expected ')' after position("));
            }
            return Ok(NumExpr::Position);
        }
        if self.peek_name() == Some("last") {
            self.position += "last".len();
            if !self.eat(b'(') {
                return Err(XPathError::Syntax("expected '(' after last"));
            }
            if !self.eat(b')') {
                return Err(XPathError::Syntax("expected ')' after last("));
            }
            return Ok(NumExpr::Last);
        }
        Ok(NumExpr::Integer(self.integer()?))
    }

    fn predicate(&mut self) -> Result<Predicate, XPathError> {
        self.skip_whitespace();
        let start = self.position;
        if self.eat(b'@') {
            let name = self.name()?.to_owned();
            if self.string_equality_ahead() {
                self.skip_whitespace();
                if !self.eat(b'=') {
                    return Err(XPathError::Syntax("expected '=' in attribute predicate"));
                }
                self.skip_whitespace();
                let value = self.literal()?;
                self.skip_whitespace();
                return Ok(Predicate::Attribute { name, value });
            }
            self.position = start;
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
        let left = self.numeric_expression()?;
        self.skip_whitespace();
        if self.at_end() || self.bytes.get(self.position) == Some(&b']') {
            if let NumExpr::Integer(value) = &left {
                return usize::try_from(*value)
                    .ok()
                    .filter(|position| *position >= 1)
                    .map(Predicate::Position)
                    .ok_or(XPathError::Syntax("a position is one-based"));
            }
            return Err(XPathError::Syntax("expected a comparison"));
        }
        let comparison = self.comparison()?;
        let right = self.numeric_expression()?;
        self.skip_whitespace();
        Ok(Predicate::Numeric {
            left,
            comparison,
            right,
        })
    }
}
