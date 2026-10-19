//! Parses source text into statements.

use crate::lexer::{Tok, Token, tokenize};
use crate::{
    Effect, FailureKind, KEYWORDS, MAX_BLOCK_DEPTH, MAX_EXPRESSION_DEPTH, MAX_SOURCE_BYTES,
    MAX_SOURCE_LINES, Program, ScriptFailure, Span, Verb,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Builtin {
    Program,
    Preview,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnaryOp {
    Neg,
    Not,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExprKind {
    Int(i64),
    Str(String),
    Bool(bool),
    Var(String),
    Builtin(Builtin),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Expr {
    pub(crate) kind: ExprKind,
    pub(crate) at: Span,
    /// Nodes on the longest path from this node to a leaf, so evaluating
    /// it recurses at most this deep.
    pub(crate) height: usize,
}

impl Expr {
    const fn leaf(kind: ExprKind, at: Span) -> Self {
        Self {
            kind,
            at,
            height: 1,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StatementKind {
    Let {
        name: String,
        value: Expr,
    },
    /// `if` and its `elif`s as one flat list of `(where, condition, body)`
    /// arms, so a long `elif` chain never nests.
    If {
        arms: Vec<(Span, Expr, Vec<Statement>)>,
        otherwise: Vec<Statement>,
    },
    Repeat {
        count: Expr,
        body: Vec<Statement>,
    },
    While {
        condition: Expr,
        body: Vec<Statement>,
    },
    Stop,
    Action {
        verb: Verb,
        effect: Option<Effect>,
        arguments: Vec<Expr>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Statement {
    pub(crate) kind: StatementKind,
    pub(crate) at: Span,
}

fn syntax(at: Span, message: impl Into<String>) -> ScriptFailure {
    ScriptFailure::new(FailureKind::Syntax, at, message)
}

/// One non-empty source line.
struct Line {
    tokens: Vec<Token>,
}

/// Parses `source`.
///
/// # Errors
///
/// Returns the first syntax error, with its line and column.
pub fn parse(source: &str) -> Result<Program, ScriptFailure> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(syntax(
            Span::new(1, 1),
            format!("the script is longer than {MAX_SOURCE_BYTES} bytes"),
        ));
    }
    let mut lines = Vec::new();
    let segments = source.split('\n').count();
    for (index, text) in source.split('\n').enumerate() {
        let number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        // A final newline ends the last line; it does not start another.
        let final_newline = index > 0 && index + 1 == segments && text.is_empty();
        if index >= MAX_SOURCE_LINES && !final_newline {
            return Err(syntax(
                Span::new(number, 1),
                format!("the script has more than {MAX_SOURCE_LINES} lines"),
            ));
        }
        let tokens = tokenize(number, text)?;
        if !tokens.is_empty() {
            lines.push(Line { tokens });
        }
    }
    let mut parser = Parser {
        lines,
        next: 0,
        end_line: u32::try_from(segments).unwrap_or(u32::MAX),
    };
    let (statements, terminator) = parser.block(0, &[])?;
    debug_assert!(terminator.is_none());
    Ok(Program { statements })
}

struct Parser {
    lines: Vec<Line>,
    next: usize,
    end_line: u32,
}

/// The keyword that closed a block and where it stood.
type Terminator = Option<(&'static str, Span, Vec<Token>)>;

impl Parser {
    /// Parses statements until one of `terminators` (returned with the rest
    /// of its line) or the end of the source (only allowed at depth 0).
    fn block(
        &mut self,
        depth: usize,
        terminators: &[&'static str],
    ) -> Result<(Vec<Statement>, Terminator), ScriptFailure> {
        let mut statements = Vec::new();
        while self.next < self.lines.len() {
            let tokens = self.lines[self.next].tokens.clone();
            self.next += 1;
            let head = &tokens[0];
            let Tok::Ident(word) = &head.tok else {
                return Err(syntax(
                    head.at,
                    format!("expected a statement, found {}", head.tok.describe()),
                ));
            };
            let rest = &tokens[1..];
            match word.as_str() {
                word @ ("end" | "else" | "elif") => {
                    let Some(terminator) = terminators.iter().find(|candidate| **candidate == word)
                    else {
                        return Err(syntax(head.at, format!("unexpected {word}")));
                    };
                    return Ok((statements, Some((terminator, head.at, rest.to_vec()))));
                }
                "if" => statements.push(self.if_statement(depth, head.at, rest)?),
                "repeat" | "while" => {
                    if depth + 1 > MAX_BLOCK_DEPTH {
                        return Err(syntax(
                            head.at,
                            format!("blocks nest deeper than {MAX_BLOCK_DEPTH}"),
                        ));
                    }
                    let condition = whole_expression(head.at, rest)?;
                    let (body, closed) = self.block(depth + 1, &["end"])?;
                    let Some((_, _, trailing)) = closed else {
                        return Err(self.missing_end(word, head.at));
                    };
                    no_trailing("end", &trailing)?;
                    let kind = if word == "repeat" {
                        StatementKind::Repeat {
                            count: condition,
                            body,
                        }
                    } else {
                        StatementKind::While { condition, body }
                    };
                    statements.push(Statement { kind, at: head.at });
                }
                "let" => statements.push(let_statement(head.at, rest)?),
                "stop" => {
                    no_trailing("stop", rest)?;
                    statements.push(Statement {
                        kind: StatementKind::Stop,
                        at: head.at,
                    });
                }
                other => {
                    let Some(verb) = Verb::from_word(other) else {
                        return Err(syntax(head.at, format!("unknown statement {other}")));
                    };
                    statements.push(action_statement(verb, head.at, rest)?);
                }
            }
        }
        Ok((statements, None))
    }

    fn missing_end(&self, word: &str, at: Span) -> ScriptFailure {
        syntax(
            Span::new(self.end_line, 1),
            format!("the {word} on line {} has no end", at.line),
        )
    }

    fn if_statement(
        &mut self,
        depth: usize,
        at: Span,
        rest: &[Token],
    ) -> Result<Statement, ScriptFailure> {
        if depth + 1 > MAX_BLOCK_DEPTH {
            return Err(syntax(
                at,
                format!("blocks nest deeper than {MAX_BLOCK_DEPTH}"),
            ));
        }
        let mut arm = (at, whole_expression(at, rest)?);
        let mut arms = Vec::new();
        let otherwise = loop {
            let (body, closed) = self.block(depth + 1, &["elif", "else", "end"])?;
            let Some((word, closed_at, trailing)) = closed else {
                return Err(self.missing_end("if", at));
            };
            arms.push((arm.0, arm.1, body));
            match word {
                "end" => {
                    no_trailing("end", &trailing)?;
                    break Vec::new();
                }
                "else" => {
                    no_trailing("else", &trailing)?;
                    let (otherwise, closed) = self.block(depth + 1, &["end"])?;
                    let Some((_, _, trailing)) = closed else {
                        return Err(self.missing_end("if", at));
                    };
                    no_trailing("end", &trailing)?;
                    break otherwise;
                }
                _ => arm = (closed_at, whole_expression(closed_at, &trailing)?),
            }
        };
        Ok(Statement {
            kind: StatementKind::If { arms, otherwise },
            at,
        })
    }
}

fn no_trailing(word: &str, rest: &[Token]) -> Result<(), ScriptFailure> {
    match rest.first() {
        None => Ok(()),
        Some(token) => Err(syntax(
            token.at,
            format!("unexpected {} after {word}", token.tok.describe()),
        )),
    }
}

fn is_reserved(word: &str) -> bool {
    KEYWORDS.contains(&word) || Verb::from_word(word).is_some()
}

fn let_statement(at: Span, rest: &[Token]) -> Result<Statement, ScriptFailure> {
    let Some(Token {
        tok: Tok::Ident(name),
        at: name_at,
    }) = rest.first()
    else {
        let at = rest.first().map_or(at, |token| token.at);
        return Err(syntax(at, "expected a variable name after let"));
    };
    if is_reserved(name) {
        return Err(syntax(*name_at, format!("{name} is a reserved word")));
    }
    match rest.get(1) {
        Some(Token {
            tok: Tok::Assign, ..
        }) => {}
        Some(token) => {
            return Err(syntax(
                token.at,
                format!("expected = after {name}, found {}", token.tok.describe()),
            ));
        }
        None => return Err(syntax(*name_at, format!("expected = after {name}"))),
    }
    let value = whole_expression(rest[1].at, &rest[2..])?;
    Ok(Statement {
        kind: StatementKind::Let {
            name: name.clone(),
            value,
        },
        at,
    })
}

fn action_statement(verb: Verb, at: Span, rest: &[Token]) -> Result<Statement, ScriptFailure> {
    let mut rest = rest;
    let mut effect = None;
    if verb == Verb::Transition {
        let Some(Token {
            tok: Tok::Ident(word),
            at: word_at,
        }) = rest.first()
        else {
            return Err(syntax(
                rest.first().map_or(at, |token| token.at),
                "expected an effect name after transition",
            ));
        };
        effect = Some(
            Effect::from_word(word)
                .ok_or_else(|| syntax(*word_at, format!("unknown transition effect {word}")))?,
        );
        match rest.get(1) {
            None => rest = &rest[1..],
            Some(Token {
                tok: Tok::Comma, ..
            }) => rest = &rest[2..],
            Some(token) => {
                return Err(syntax(
                    token.at,
                    format!("expected , after {word}, found {}", token.tok.describe()),
                ));
            }
        }
    }
    let mut arguments = Vec::new();
    if !rest.is_empty() {
        let mut cursor = Cursor {
            tokens: rest,
            next: 0,
            depth: 0,
            fallback: at,
        };
        loop {
            arguments.push(cursor.expression()?);
            match cursor.peek() {
                None => break,
                Some(Token {
                    tok: Tok::Comma,
                    at: comma_at,
                }) => {
                    let comma_at = *comma_at;
                    cursor.next += 1;
                    if cursor.peek().is_none() {
                        return Err(syntax(comma_at, "expected an argument after ,"));
                    }
                }
                Some(token) => {
                    return Err(syntax(
                        token.at,
                        format!("unexpected {}", token.tok.describe()),
                    ));
                }
            }
        }
    }
    let (minimum, maximum) = verb.arity();
    if arguments.len() < minimum || arguments.len() > maximum {
        let expected = if minimum == maximum {
            format!("{minimum}")
        } else {
            format!("{minimum} to {maximum}")
        };
        return Err(syntax(
            at,
            format!(
                "{} takes {expected} argument{}, found {}",
                verb.word(),
                if maximum == 1 { "" } else { "s" },
                arguments.len()
            ),
        ));
    }
    Ok(Statement {
        kind: StatementKind::Action {
            verb,
            effect,
            arguments,
        },
        at,
    })
}

/// Parses `tokens` as exactly one expression; `at` locates an empty one.
fn whole_expression(at: Span, tokens: &[Token]) -> Result<Expr, ScriptFailure> {
    let mut cursor = Cursor {
        tokens,
        next: 0,
        depth: 0,
        fallback: at,
    };
    let expression = cursor.expression()?;
    if let Some(token) = cursor.peek() {
        return Err(syntax(
            token.at,
            format!("unexpected {}", token.tok.describe()),
        ));
    }
    Ok(expression)
}

struct Cursor<'a> {
    tokens: &'a [Token],
    next: usize,
    depth: usize,
    /// Where an expression missing at the end of the line is reported.
    fallback: Span,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.next)
    }

    fn end_span(&self) -> Span {
        self.tokens.last().map_or(self.fallback, |token| token.at)
    }

    fn enter(&mut self, at: Span) -> Result<(), ScriptFailure> {
        self.depth += 1;
        if self.depth > MAX_EXPRESSION_DEPTH {
            return Err(syntax(
                at,
                format!("the expression nests deeper than {MAX_EXPRESSION_DEPTH}"),
            ));
        }
        Ok(())
    }

    fn is_word(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token { tok: Tok::Ident(found), .. }) if found == word)
    }

    fn expression(&mut self) -> Result<Expr, ScriptFailure> {
        let at = self.peek().map_or(self.fallback, |token| token.at);
        let saved = self.depth;
        self.enter(at)?;
        let result = self.or();
        self.depth = saved;
        result
    }

    /// The height of a node over `children`, refused past
    /// [`MAX_EXPRESSION_DEPTH`]. The nesting counter alone does not bound
    /// the tree: a chain of operators opened by a parenthesized operand puts
    /// that operand's whole tree below the chain.
    fn height(at: Span, children: &[&Expr]) -> Result<usize, ScriptFailure> {
        let height = 1 + children.iter().map(|child| child.height).max().unwrap_or(0);
        if height > MAX_EXPRESSION_DEPTH {
            return Err(syntax(
                at,
                format!("the expression nests deeper than {MAX_EXPRESSION_DEPTH}"),
            ));
        }
        Ok(height)
    }

    fn binary(op: BinaryOp, left: Expr, right: Expr, at: Span) -> Result<Expr, ScriptFailure> {
        Ok(Expr {
            height: Self::height(at, &[&left, &right])?,
            kind: ExprKind::Binary(op, Box::new(left), Box::new(right)),
            at,
        })
    }

    fn unary_node(op: UnaryOp, operand: Expr, at: Span) -> Result<Expr, ScriptFailure> {
        Ok(Expr {
            height: Self::height(at, &[&operand])?,
            kind: ExprKind::Unary(op, Box::new(operand)),
            at,
        })
    }

    fn or(&mut self) -> Result<Expr, ScriptFailure> {
        let mut left = self.and()?;
        while self.is_word("or") {
            let at = self.tokens[self.next].at;
            self.next += 1;
            self.enter(at)?;
            let right = self.and()?;
            left = Self::binary(BinaryOp::Or, left, right, at)?;
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, ScriptFailure> {
        let mut left = self.not()?;
        while self.is_word("and") {
            let at = self.tokens[self.next].at;
            self.next += 1;
            self.enter(at)?;
            let right = self.not()?;
            left = Self::binary(BinaryOp::And, left, right, at)?;
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Expr, ScriptFailure> {
        if self.is_word("not") {
            let at = self.tokens[self.next].at;
            self.next += 1;
            self.enter(at)?;
            let operand = self.not()?;
            return Self::unary_node(UnaryOp::Not, operand, at);
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Expr, ScriptFailure> {
        let left = self.sum()?;
        let op = match self.peek().map(|token| &token.tok) {
            Some(Tok::EqEq) => BinaryOp::Eq,
            Some(Tok::NotEq) => BinaryOp::Ne,
            Some(Tok::Lt) => BinaryOp::Lt,
            Some(Tok::Le) => BinaryOp::Le,
            Some(Tok::Gt) => BinaryOp::Gt,
            Some(Tok::Ge) => BinaryOp::Ge,
            Some(Tok::Assign) => {
                return Err(syntax(
                    self.tokens[self.next].at,
                    "use == to compare (= only follows let)",
                ));
            }
            _ => return Ok(left),
        };
        let at = self.tokens[self.next].at;
        self.next += 1;
        let right = self.sum()?;
        Self::binary(op, left, right, at)
    }

    fn sum(&mut self) -> Result<Expr, ScriptFailure> {
        let mut left = self.term()?;
        loop {
            let op = match self.peek().map(|token| &token.tok) {
                Some(Tok::Plus) => BinaryOp::Add,
                Some(Tok::Minus) => BinaryOp::Sub,
                _ => return Ok(left),
            };
            let at = self.tokens[self.next].at;
            self.next += 1;
            self.enter(at)?;
            let right = self.term()?;
            left = Self::binary(op, left, right, at)?;
        }
    }

    fn term(&mut self) -> Result<Expr, ScriptFailure> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek().map(|token| &token.tok) {
                Some(Tok::Star) => BinaryOp::Mul,
                Some(Tok::Slash) => BinaryOp::Div,
                Some(Tok::Percent) => BinaryOp::Rem,
                _ => return Ok(left),
            };
            let at = self.tokens[self.next].at;
            self.next += 1;
            self.enter(at)?;
            let right = self.unary()?;
            left = Self::binary(op, left, right, at)?;
        }
    }

    fn unary(&mut self) -> Result<Expr, ScriptFailure> {
        if let Some(Token {
            tok: Tok::Minus,
            at,
        }) = self.peek()
        {
            let at = *at;
            self.next += 1;
            self.enter(at)?;
            // A negative literal folds, so `-9223372036854775808` is a value.
            if let Some(Token {
                tok: Tok::Int(value),
                ..
            }) = self.peek()
            {
                let value = 0_i64.checked_sub_unsigned(*value).ok_or_else(|| {
                    syntax(self.tokens[self.next].at, format!("-{value} is too large"))
                })?;
                self.next += 1;
                return Ok(Expr::leaf(ExprKind::Int(value), at));
            }
            let operand = self.unary()?;
            return Self::unary_node(UnaryOp::Neg, operand, at);
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<Expr, ScriptFailure> {
        let Some(token) = self.peek().cloned() else {
            return Err(syntax(self.end_span(), "expected a value"));
        };
        self.next += 1;
        let kind = match token.tok {
            Tok::Int(value) => ExprKind::Int(
                i64::try_from(value)
                    .map_err(|_| syntax(token.at, format!("{value} is too large")))?,
            ),
            Tok::Str(value) => ExprKind::Str(value),
            Tok::Ident(word) => match word.as_str() {
                "true" => ExprKind::Bool(true),
                "false" => ExprKind::Bool(false),
                "program" => ExprKind::Builtin(Builtin::Program),
                "preview" => ExprKind::Builtin(Builtin::Preview),
                word if is_reserved(word) => {
                    return Err(syntax(token.at, format!("unexpected {word}")));
                }
                _ => ExprKind::Var(word),
            },
            Tok::LParen => {
                let saved = self.depth;
                self.enter(token.at)?;
                let inner = self.or()?;
                self.depth = saved;
                match self.peek() {
                    Some(Token {
                        tok: Tok::RParen, ..
                    }) => self.next += 1,
                    Some(other) => {
                        return Err(syntax(
                            other.at,
                            format!("expected ), found {}", other.tok.describe()),
                        ));
                    }
                    None => return Err(syntax(token.at, "this ( is never closed")),
                }
                return Ok(inner);
            }
            other => {
                return Err(syntax(
                    token.at,
                    format!("expected a value, found {}", other.describe()),
                ));
            }
        };
        Ok(Expr::leaf(kind, token.at))
    }
}
