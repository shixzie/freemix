//! Splits one source line into tokens.

use crate::{FailureKind, ScriptFailure, Span};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Tok {
    /// A literal's magnitude, at most `2^63`: only a leading `-` brings that
    /// one into range (`-9223372036854775808`), and the parser refuses it
    /// anywhere else.
    Int(u64),
    Str(String),
    Ident(String),
    Comma,
    LParen,
    RParen,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Tok {
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Int(value) => value.to_string(),
            Self::Str(_) => "a string".to_owned(),
            Self::Ident(word) => word.clone(),
            Self::Comma => ",".to_owned(),
            Self::LParen => "(".to_owned(),
            Self::RParen => ")".to_owned(),
            Self::Plus => "+".to_owned(),
            Self::Minus => "-".to_owned(),
            Self::Star => "*".to_owned(),
            Self::Slash => "/".to_owned(),
            Self::Percent => "%".to_owned(),
            Self::Assign => "=".to_owned(),
            Self::EqEq => "==".to_owned(),
            Self::NotEq => "!=".to_owned(),
            Self::Lt => "<".to_owned(),
            Self::Le => "<=".to_owned(),
            Self::Gt => ">".to_owned(),
            Self::Ge => ">=".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    pub(crate) tok: Tok,
    pub(crate) at: Span,
}

/// Whether `word` lexes as one name: an ASCII letter or `_`, then ASCII
/// letters, digits, or `_`.
pub(crate) fn is_identifier(word: &str) -> bool {
    let mut characters = word.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn column(index: usize) -> u32 {
    u32::try_from(index + 1).unwrap_or(u32::MAX)
}

/// Tokenizes `text`, line `line` of the source. A `#` outside a string
/// starts a comment.
pub(crate) fn tokenize(line: u32, text: &str) -> Result<Vec<Token>, ScriptFailure> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    let error = |index: usize, message: String| {
        ScriptFailure::new(FailureKind::Syntax, Span::new(line, column(index)), message)
    };
    while index < chars.len() {
        let start = index;
        let at = Span::new(line, column(start));
        let character = chars[index];
        if character == ' ' || character == '\t' || character == '\r' {
            index += 1;
            continue;
        }
        if character == '#' {
            break;
        }
        let tok = if character.is_ascii_digit() {
            while index < chars.len() && chars[index].is_ascii_digit() {
                index += 1;
            }
            if index < chars.len() && (chars[index].is_ascii_alphabetic() || chars[index] == '_') {
                return Err(error(index, "a number must not run into a name".to_owned()));
            }
            let digits: String = chars[start..index].iter().collect();
            let value = digits
                .parse::<u64>()
                .ok()
                .filter(|value| *value <= i64::MIN.unsigned_abs())
                .ok_or_else(|| error(start, format!("{digits} is too large")))?;
            tokens.push(Token {
                tok: Tok::Int(value),
                at,
            });
            continue;
        } else if character.is_ascii_alphabetic() || character == '_' {
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric() || chars[index] == '_')
            {
                index += 1;
            }
            let word: String = chars[start..index].iter().collect();
            tokens.push(Token {
                tok: Tok::Ident(word),
                at,
            });
            continue;
        } else if character == '"' {
            let (value, after) = string(&chars, start, line)?;
            index = after;
            tokens.push(Token {
                tok: Tok::Str(value),
                at,
            });
            continue;
        } else {
            let next = chars.get(index + 1).copied();
            let (tok, width) = match (character, next) {
                ('=', Some('=')) => (Tok::EqEq, 2),
                ('!', Some('=')) => (Tok::NotEq, 2),
                ('<', Some('=')) => (Tok::Le, 2),
                ('>', Some('=')) => (Tok::Ge, 2),
                ('=', _) => (Tok::Assign, 1),
                ('<', _) => (Tok::Lt, 1),
                ('>', _) => (Tok::Gt, 1),
                (',', _) => (Tok::Comma, 1),
                ('(', _) => (Tok::LParen, 1),
                (')', _) => (Tok::RParen, 1),
                ('+', _) => (Tok::Plus, 1),
                ('-', _) => (Tok::Minus, 1),
                ('*', _) => (Tok::Star, 1),
                ('/', _) => (Tok::Slash, 1),
                ('%', _) => (Tok::Percent, 1),
                (other, _) => {
                    return Err(error(
                        start,
                        format!("unexpected character {}", other.escape_debug()),
                    ));
                }
            };
            index += width;
            tok
        };
        tokens.push(Token { tok, at });
    }
    Ok(tokens)
}

/// Lexes the string literal opening at `start`; returns its value and the
/// index after its closing quote.
fn string(chars: &[char], start: usize, line: u32) -> Result<(String, usize), ScriptFailure> {
    let error = |index: usize, message: &str| {
        ScriptFailure::new(FailureKind::Syntax, Span::new(line, column(index)), message)
    };
    let mut index = start + 1;
    let mut value = String::new();
    loop {
        let Some(&next) = chars.get(index) else {
            return Err(error(start, "unterminated string"));
        };
        index += 1;
        match next {
            '"' => return Ok((value, index)),
            '\\' => match chars.get(index) {
                Some(&escaped @ ('"' | '\\')) => {
                    value.push(escaped);
                    index += 1;
                }
                _ => return Err(error(index - 1, "only \\\" and \\\\ escapes are allowed")),
            },
            other if other.is_control() => {
                return Err(error(index - 1, "control character in string"));
            }
            other => value.push(other),
        }
    }
}
