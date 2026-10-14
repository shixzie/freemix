//! Editable shortcut templates with dynamic targets and text import/export.
//!
//! A template is the editable description of a shortcut: the global or local
//! scope, the chord, and the conflict rules of [`crate::Shortcut`] plus a
//! display name and a target that is resolved when the binding is pressed.
//! Templates are also the unit of import and export, serialized as a
//! versioned, tab-separated text profile so an operator can move a keymap
//! between machines without coupling the profile to the command type.

use crate::{
    Chord, CommandIntent, KeyStroke, Modifiers, ShortcutConflict, ShortcutScope, ValueRange,
};
use core::fmt;
use fm_types::InputId;
use std::num::NonZeroU128;

/// Maximum number of templates a [`ShortcutTemplateRegistry`] or an imported
/// profile may contain.
pub const MAX_TEMPLATES: usize = 512;

/// Maximum byte length of a template identifier or display name.
pub const MAX_TEMPLATE_NAME_BYTES: usize = 128;

/// Maximum byte length of a stream name carried by an intent or a value target.
pub const MAX_STREAM_NAME_BYTES: usize = 128;

/// Maximum byte length of one line of a shortcut profile.
pub const MAX_PROFILE_LINE_BYTES: usize = 512;

/// First field of the header line of a shortcut profile.
pub const SHORTCUT_PROFILE_MAGIC: &str = "freemix-shortcuts";

/// Version field of the header line of a shortcut profile.
pub const SHORTCUT_PROFILE_VERSION: &str = "1";

/// Number of tab-separated fields on a profile template line.
const PROFILE_FIELDS: usize = 9;

/// Selects the input a template acts on when the identity is not static.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InputSelector {
    /// The `slot`-th input of the roster supplied at press time. Slots are
    /// 1-based, so slot `0` is invalid.
    BySlot { slot: u16 },
    /// A fixed input identity that never consults the press-time roster.
    ByInput(InputId),
}

/// Describes what a template needs from the caller when it is pressed.
#[derive(Clone, Debug, PartialEq)]
pub enum TemplateTarget {
    /// The template is complete; no press-time payload is accepted.
    Static,
    /// The input identity is supplied at press time.
    Input(InputSelector),
    /// The value is supplied at press time and validated against `range`.
    Value { stream: String, range: ValueRange },
}

/// A named shortcut whose dynamic target is resolved at press time.
#[derive(Clone, Debug, PartialEq)]
pub struct ShortcutTemplate<C> {
    pub id: String,
    pub name: String,
    pub scope: ShortcutScope,
    pub chord: Chord,
    pub intent: CommandIntent<C>,
    pub target: TemplateTarget,
}

impl<C> ShortcutTemplate<C> {
    /// Creates a template and validates every field the profile relies on.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or oversized identifier, name, local
    /// scope, or stream; for a tab, carriage return, or newline in any of
    /// them; for a zero [`InputSelector::BySlot`] slot; or for a non-finite
    /// or non-increasing [`TemplateTarget::Value`] range.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        scope: ShortcutScope,
        chord: Chord,
        intent: CommandIntent<C>,
        target: TemplateTarget,
    ) -> Result<Self, TemplateError> {
        let template = Self {
            id: id.into(),
            name: name.into(),
            scope,
            chord,
            intent,
            target,
        };
        template.validate()?;
        Ok(template)
    }

    /// Validates the template without consuming it.
    ///
    /// # Errors
    ///
    /// Returns the first invalid field.
    pub fn validate(&self) -> Result<(), TemplateError> {
        if self.id.is_empty() {
            return Err(TemplateError::EmptyId);
        }
        if self.id.len() > MAX_TEMPLATE_NAME_BYTES {
            return Err(TemplateError::IdTooLong {
                maximum: MAX_TEMPLATE_NAME_BYTES,
            });
        }
        if contains_line_break(&self.id) {
            return Err(TemplateError::InvalidText { field: "id" });
        }
        if self.name.is_empty() {
            return Err(TemplateError::EmptyName);
        }
        if self.name.len() > MAX_TEMPLATE_NAME_BYTES {
            return Err(TemplateError::NameTooLong {
                maximum: MAX_TEMPLATE_NAME_BYTES,
            });
        }
        if contains_line_break(&self.name) {
            return Err(TemplateError::InvalidText { field: "name" });
        }
        if let ShortcutScope::Local(local) = &self.scope {
            if local.is_empty() {
                return Err(TemplateError::EmptyScope);
            }
            if local.len() > MAX_TEMPLATE_NAME_BYTES {
                return Err(TemplateError::ScopeTooLong {
                    maximum: MAX_TEMPLATE_NAME_BYTES,
                });
            }
            if contains_line_break(local) {
                return Err(TemplateError::InvalidText { field: "scope" });
            }
        }
        match &self.intent {
            CommandIntent::Discrete(_) => {}
            CommandIntent::Continuous { stream, .. }
            | CommandIntent::CommitContinuous { stream, .. } => {
                validate_stream(stream)?;
            }
        }
        match &self.target {
            TemplateTarget::Static => {}
            TemplateTarget::Input(selector) => {
                if let InputSelector::BySlot { slot: 0 } = selector {
                    return Err(TemplateError::InvalidSlot { slot: 0 });
                }
            }
            TemplateTarget::Value { stream, range } => {
                validate_stream(stream)?;
                if !range.minimum.is_finite()
                    || !range.maximum.is_finite()
                    || range.minimum >= range.maximum
                {
                    return Err(TemplateError::InvalidRange);
                }
            }
        }
        Ok(())
    }
}

/// The resolved dispatch payload of one template press.
///
/// Every field the caller needs to dispatch the intent is carried by value, so
/// the registry borrow ends before dispatch.
#[derive(Clone, Debug, PartialEq)]
pub struct TemplatePress<C> {
    pub template_id: String,
    pub name: String,
    pub scope: ShortcutScope,
    pub chord: Chord,
    pub intent: CommandIntent<C>,
    /// The input resolved from the template target, when it acts on an input.
    pub resolved_input: Option<InputId>,
    /// The value resolved from the template target, when it acts on a value.
    pub resolved_value: Option<f64>,
}

/// Every way a template field or a template press can be rejected.
#[derive(Clone, Debug, PartialEq)]
pub enum TemplateError {
    EmptyId,
    IdTooLong {
        maximum: usize,
    },
    EmptyName,
    NameTooLong {
        maximum: usize,
    },
    EmptyScope,
    ScopeTooLong {
        maximum: usize,
    },
    EmptyStream,
    StreamTooLong {
        maximum: usize,
    },
    InvalidText {
        field: &'static str,
    },
    InvalidRange,
    InvalidSlot {
        slot: u16,
    },
    UnknownTemplate(String),
    InactiveTemplate(String),
    UnknownChord,
    EmptyInputRoster,
    SlotOutOfRange {
        slot: u16,
        roster_len: usize,
    },
    MissingValue,
    UnexpectedInput,
    UnexpectedValue,
    NonFiniteValue,
    ValueOutOfRange {
        value: f64,
        minimum: f64,
        maximum: f64,
    },
}

impl fmt::Display for TemplateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId => formatter.write_str("shortcut template id must not be empty"),
            Self::IdTooLong { maximum } => {
                write!(
                    formatter,
                    "shortcut template id may contain at most {maximum} bytes"
                )
            }
            Self::EmptyName => formatter.write_str("shortcut template name must not be empty"),
            Self::NameTooLong { maximum } => {
                write!(
                    formatter,
                    "shortcut template name may contain at most {maximum} bytes"
                )
            }
            Self::EmptyScope => formatter.write_str("shortcut template scope must not be empty"),
            Self::ScopeTooLong { maximum } => {
                write!(
                    formatter,
                    "shortcut template scope may contain at most {maximum} bytes"
                )
            }
            Self::EmptyStream => formatter.write_str("shortcut template stream must not be empty"),
            Self::StreamTooLong { maximum } => {
                write!(
                    formatter,
                    "shortcut template stream may contain at most {maximum} bytes"
                )
            }
            Self::InvalidText { field } => {
                write!(
                    formatter,
                    "shortcut template {field} must not contain a tab, carriage return, or newline"
                )
            }
            Self::InvalidRange => {
                formatter.write_str("shortcut template value range must be finite and increasing")
            }
            Self::InvalidSlot { slot } => {
                write!(
                    formatter,
                    "shortcut template input slot {slot} is not 1-based"
                )
            }
            Self::UnknownTemplate(id) => write!(formatter, "shortcut template {id} is unknown"),
            Self::InactiveTemplate(id) => {
                write!(
                    formatter,
                    "shortcut template {id} is not active in this scope"
                )
            }
            Self::UnknownChord => {
                formatter.write_str("no shortcut template is bound to the requested chord")
            }
            Self::EmptyInputRoster => {
                formatter.write_str("a slot target requires a nonempty input roster")
            }
            Self::SlotOutOfRange { slot, roster_len } => {
                write!(
                    formatter,
                    "input slot {slot} is outside the {roster_len}-input roster"
                )
            }
            Self::MissingValue => formatter.write_str("a value target requires a press-time value"),
            Self::UnexpectedInput => {
                formatter.write_str("this shortcut template target accepts no input roster")
            }
            Self::UnexpectedValue => {
                formatter.write_str("this shortcut template target accepts no press-time value")
            }
            Self::NonFiniteValue => formatter.write_str("a press-time value must be finite"),
            Self::ValueOutOfRange {
                value,
                minimum,
                maximum,
            } => write!(
                formatter,
                "press-time value {value} is outside [{minimum}, {maximum}]"
            ),
        }
    }
}

impl std::error::Error for TemplateError {}

/// Every way an insert into a [`ShortcutTemplateRegistry`] can fail.
#[derive(Clone, Debug, PartialEq)]
pub enum TemplateRegistryError {
    InvalidTemplate(TemplateError),
    DuplicateId(String),
    TooManyTemplates { maximum: usize },
    Conflict(Vec<ShortcutConflict>),
}

impl fmt::Display for TemplateRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTemplate(error) => write!(formatter, "invalid shortcut template: {error}"),
            Self::DuplicateId(id) => write!(formatter, "shortcut template {id} already exists"),
            Self::TooManyTemplates { maximum } => {
                write!(
                    formatter,
                    "at most {maximum} shortcut templates are supported"
                )
            }
            Self::Conflict(conflicts) => {
                write!(
                    formatter,
                    "shortcut template conflicts with {} binding(s)",
                    conflicts.len()
                )
            }
        }
    }
}

impl std::error::Error for TemplateRegistryError {}

/// A bounded set of templates that share the chord and scope conflict rules of
/// [`crate::ShortcutRegistry`].
#[derive(Clone, Debug)]
pub struct ShortcutTemplateRegistry<C> {
    templates: Vec<ShortcutTemplate<C>>,
}

impl<C> Default for ShortcutTemplateRegistry<C> {
    fn default() -> Self {
        Self {
            templates: Vec::new(),
        }
    }
}

impl<C> ShortcutTemplateRegistry<C> {
    /// Templates in insertion order.
    #[must_use]
    pub fn templates(&self) -> &[ShortcutTemplate<C>] {
        &self.templates
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.templates.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ShortcutTemplate<C>> {
        self.templates.iter().find(|entry| entry.id == id)
    }

    /// Reports the exact and prefix chord conflicts an insert would hit.
    #[must_use]
    pub fn conflicts(&self, incoming: &ShortcutTemplate<C>) -> Vec<ShortcutConflict> {
        self.templates
            .iter()
            .filter(|existing| existing.scope.overlaps(&incoming.scope))
            .filter_map(|existing| {
                existing
                    .chord
                    .conflict(&incoming.chord)
                    .map(|kind| ShortcutConflict {
                        existing_id: existing.id.clone(),
                        incoming_id: incoming.id.clone(),
                        kind,
                    })
            })
            .collect()
    }

    /// Adds a validated template if its identifier, capacity, and active chord
    /// are unambiguous.
    ///
    /// # Errors
    ///
    /// Returns the same exact/prefix chord conflict details as
    /// [`crate::ShortcutRegistry::insert`], an already-used identifier, an
    /// invalid template, or the [`MAX_TEMPLATES`] capacity limit.
    pub fn insert(&mut self, template: ShortcutTemplate<C>) -> Result<(), TemplateRegistryError> {
        template
            .validate()
            .map_err(TemplateRegistryError::InvalidTemplate)?;
        if self.templates.iter().any(|entry| entry.id == template.id) {
            return Err(TemplateRegistryError::DuplicateId(template.id));
        }
        if self.templates.len() >= MAX_TEMPLATES {
            return Err(TemplateRegistryError::TooManyTemplates {
                maximum: MAX_TEMPLATES,
            });
        }
        let conflicts = self.conflicts(&template);
        if !conflicts.is_empty() {
            return Err(TemplateRegistryError::Conflict(conflicts));
        }
        self.templates.push(template);
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Option<ShortcutTemplate<C>> {
        let index = self.templates.iter().position(|entry| entry.id == id)?;
        Some(self.templates.remove(index))
    }

    /// Resolves a chord to exactly one template.
    ///
    /// A local template for `scope` always wins over a global one;
    /// [`Self::insert`] refuses any other ambiguity, so the outcome never
    /// depends on iteration order. This is the same precedence as
    /// [`crate::ShortcutRegistry::resolve`].
    #[must_use]
    pub fn resolve(&self, scope: Option<&str>, chord: &Chord) -> Option<&ShortcutTemplate<C>> {
        scope
            .and_then(|scope| {
                self.templates.iter().find(|entry| {
                    entry.chord == *chord
                        && matches!(&entry.scope, ShortcutScope::Local(local) if local == scope)
                })
            })
            .or_else(|| {
                self.templates.iter().find(|entry| {
                    entry.chord == *chord && matches!(entry.scope, ShortcutScope::Global)
                })
            })
    }
}

impl<C: Clone> ShortcutTemplateRegistry<C> {
    /// Presses the template `id`, resolving its dynamic target.
    ///
    /// When `scope` is `Some`, the template must also be active in that local
    /// scope: a global template always is, and a local one must match exactly.
    /// When `scope` is `None`, pressing by explicit identifier is allowed for
    /// any template because the identifier already names one binding.
    ///
    /// A [`TemplateTarget::Input`] resolves [`InputSelector::BySlot`] against
    /// the 1-based `inputs` roster; [`InputSelector::ByInput`] never consults
    /// the roster, so it resolves even when that input is not currently
    /// listed. A [`TemplateTarget::Value`] requires a finite `value` inside
    /// its range, and the ambient roster is not consulted. A
    /// [`TemplateTarget::Static`] accepts neither a roster nor a value, so a
    /// caller cannot silently pass a payload the template ignores.
    ///
    /// # Errors
    ///
    /// Returns unknown or inactive template, roster, value, or payload errors.
    pub fn press(
        &self,
        id: &str,
        scope: Option<&str>,
        inputs: &[InputId],
        value: Option<f64>,
    ) -> Result<TemplatePress<C>, TemplateError> {
        let template = self
            .get(id)
            .ok_or_else(|| TemplateError::UnknownTemplate(id.to_owned()))?;
        if let Some(active) = scope
            && !template_scope_active(&template.scope, active)
        {
            return Err(TemplateError::InactiveTemplate(id.to_owned()));
        }
        let (resolved_input, resolved_value) = resolve_target(&template.target, inputs, value)?;
        Ok(TemplatePress {
            template_id: template.id.clone(),
            name: template.name.clone(),
            scope: template.scope.clone(),
            chord: template.chord.clone(),
            intent: template.intent.clone(),
            resolved_input,
            resolved_value,
        })
    }

    /// Resolves a chord with the registry precedence and presses the match.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError::UnknownChord`] when no template is bound to
    /// the chord, then the same errors as [`Self::press`].
    pub fn resolve_chord(
        &self,
        scope: Option<&str>,
        chord: &Chord,
        inputs: &[InputId],
        value: Option<f64>,
    ) -> Result<TemplatePress<C>, TemplateError> {
        let template = self
            .resolve(scope, chord)
            .ok_or(TemplateError::UnknownChord)?;
        let id = template.id.clone();
        self.press(&id, scope, inputs, value)
    }
}

/// Encodes commands of one command type to and from profile text.
///
/// The text profile stores each command as one opaque field, so the codec is
/// the only place that knows how a command is represented. Implementations
/// should keep encoded text free of tabs, carriage returns, and newlines: the
/// line-oriented profile reserves those bytes, so a codec that emits one makes
/// its own document unparseable.
pub trait ShortcutProfileCodec<C> {
    /// Encodes one command into a single profile field.
    fn encode_command(&self, command: &C) -> String;

    /// Decodes one profile field back into a command.
    ///
    /// # Errors
    ///
    /// Returning an error rejects the whole document; parsing never keeps a
    /// partial result.
    fn decode_command(&self, text: &str) -> Result<C, String>;
}

/// Every way a shortcut profile document can be rejected.
#[derive(Clone, Debug, PartialEq)]
pub enum ShortcutProfileError {
    MissingHeader,
    UnknownHeader(String),
    UnsupportedVersion(String),
    LineTooLong {
        line: usize,
        maximum: usize,
    },
    TooManyTemplates {
        maximum: usize,
    },
    FieldCount {
        line: usize,
        expected: usize,
        actual: usize,
    },
    InvalidField {
        line: usize,
        field: &'static str,
    },
    InvalidChord {
        line: usize,
    },
    InvalidTemplate {
        line: usize,
        error: TemplateError,
    },
    DuplicateId {
        line: usize,
        id: String,
    },
    InvalidCommand {
        line: usize,
        message: String,
    },
}

impl fmt::Display for ShortcutProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHeader => formatter.write_str("shortcut profile is missing its header"),
            Self::UnknownHeader(header) => {
                write!(
                    formatter,
                    "shortcut profile header {header} is not recognized"
                )
            }
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "shortcut profile version {version} is not supported"
                )
            }
            Self::LineTooLong { line, maximum } => {
                write!(
                    formatter,
                    "shortcut profile line {line} exceeds {maximum} bytes"
                )
            }
            Self::TooManyTemplates { maximum } => {
                write!(
                    formatter,
                    "shortcut profile may contain at most {maximum} templates"
                )
            }
            Self::FieldCount {
                line,
                expected,
                actual,
            } => write!(
                formatter,
                "shortcut profile line {line} has {actual} fields, expected {expected}"
            ),
            Self::InvalidField { line, field } => {
                write!(
                    formatter,
                    "shortcut profile line {line} has an invalid {field} field"
                )
            }
            Self::InvalidChord { line } => {
                write!(
                    formatter,
                    "shortcut profile line {line} has an invalid chord"
                )
            }
            Self::InvalidTemplate { line, error } => {
                write!(
                    formatter,
                    "shortcut profile line {line} has an invalid template: {error}"
                )
            }
            Self::DuplicateId { line, id } => {
                write!(
                    formatter,
                    "shortcut profile line {line} repeats template {id}"
                )
            }
            Self::InvalidCommand { line, message } => {
                write!(
                    formatter,
                    "shortcut profile line {line} has an invalid command: {message}"
                )
            }
        }
    }
}

impl std::error::Error for ShortcutProfileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidTemplate { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// Renders templates as a complete, versioned shortcut profile.
///
/// Each template becomes one line, so the document round-trips through
/// [`parse_shortcut_profile`] byte-for-byte when every encoded command is free
/// of tabs, carriage returns, and newlines. Rendering never fails; keep the
/// slice within [`MAX_TEMPLATES`] and each line within [`MAX_PROFILE_LINE_BYTES`].
#[must_use]
pub fn render_shortcut_profile<C>(
    templates: &[ShortcutTemplate<C>],
    codec: &impl ShortcutProfileCodec<C>,
) -> String {
    let mut profile = String::new();
    profile.push_str(SHORTCUT_PROFILE_MAGIC);
    profile.push('\t');
    profile.push_str(SHORTCUT_PROFILE_VERSION);
    profile.push('\n');
    for template in templates {
        profile.push_str(&render_template_line(template, codec));
        profile.push('\n');
    }
    profile
}

/// Parses a complete shortcut profile into templates in file order.
///
/// The first line is the [`SHORTCUT_PROFILE_MAGIC`] and
/// [`SHORTCUT_PROFILE_VERSION`] header; every following line is one template
/// with nine tab-separated fields. Parsing is all-or-nothing: any malformed
/// header, version, line, field, chord, range, or command rejects the whole
/// document and never returns a partial list.
///
/// # Errors
///
/// Returns the first [`ShortcutProfileError`] that rejects the document.
pub fn parse_shortcut_profile<C>(
    text: &str,
    codec: &impl ShortcutProfileCodec<C>,
) -> Result<Vec<ShortcutTemplate<C>>, ShortcutProfileError> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let Some((header, template_lines)) = lines.split_first() else {
        return Err(ShortcutProfileError::MissingHeader);
    };
    parse_header(header)?;
    if template_lines.len() > MAX_TEMPLATES {
        return Err(ShortcutProfileError::TooManyTemplates {
            maximum: MAX_TEMPLATES,
        });
    }
    let mut templates = Vec::with_capacity(template_lines.len());
    for (index, line) in template_lines.iter().enumerate() {
        let template = parse_template_line(index + 2, line, codec)?;
        if templates
            .iter()
            .any(|existing: &ShortcutTemplate<C>| existing.id == template.id)
        {
            return Err(ShortcutProfileError::DuplicateId {
                line: index + 2,
                id: template.id,
            });
        }
        templates.push(template);
    }
    Ok(templates)
}

fn parse_header(header: &str) -> Result<(), ShortcutProfileError> {
    if header.len() > MAX_PROFILE_LINE_BYTES {
        return Err(ShortcutProfileError::LineTooLong {
            line: 1,
            maximum: MAX_PROFILE_LINE_BYTES,
        });
    }
    let fields: Vec<&str> = header.split('\t').collect();
    if fields.len() != 2 || fields[0] != SHORTCUT_PROFILE_MAGIC {
        return Err(ShortcutProfileError::UnknownHeader(header.to_owned()));
    }
    if fields[1] != SHORTCUT_PROFILE_VERSION {
        return Err(ShortcutProfileError::UnsupportedVersion(
            fields[1].to_owned(),
        ));
    }
    Ok(())
}

fn parse_template_line<C>(
    line: usize,
    text: &str,
    codec: &impl ShortcutProfileCodec<C>,
) -> Result<ShortcutTemplate<C>, ShortcutProfileError> {
    if text.len() > MAX_PROFILE_LINE_BYTES {
        return Err(ShortcutProfileError::LineTooLong {
            line,
            maximum: MAX_PROFILE_LINE_BYTES,
        });
    }
    if text.contains('\r') {
        return Err(ShortcutProfileError::InvalidField {
            line,
            field: "line",
        });
    }
    let fields: Vec<&str> = text.split('\t').collect();
    if fields.len() != PROFILE_FIELDS {
        return Err(ShortcutProfileError::FieldCount {
            line,
            expected: PROFILE_FIELDS,
            actual: fields.len(),
        });
    }
    let invalid = |field: &'static str| ShortcutProfileError::InvalidField { line, field };
    let id = decode_field(fields[0]).ok_or_else(|| invalid("id"))?;
    let name = decode_field(fields[1]).ok_or_else(|| invalid("name"))?;
    let scope = parse_scope(fields[2], line)?;
    let chord = parse_chord(fields[3], line)?;
    let command = codec
        .decode_command(fields[8])
        .map_err(|message| ShortcutProfileError::InvalidCommand { line, message })?;
    let intent = match fields[4] {
        "discrete" => {
            if !fields[5].is_empty() {
                return Err(invalid("stream"));
            }
            CommandIntent::Discrete(command)
        }
        "continuous" => CommandIntent::Continuous {
            stream: decode_field(fields[5]).ok_or_else(|| invalid("stream"))?,
            command,
        },
        "commit" => CommandIntent::CommitContinuous {
            stream: decode_field(fields[5]).ok_or_else(|| invalid("stream"))?,
            command,
        },
        _ => return Err(invalid("delivery")),
    };
    let target = parse_target(fields[6], fields[7], line)?;
    ShortcutTemplate::new(id, name, scope, chord, intent, target)
        .map_err(|error| ShortcutProfileError::InvalidTemplate { line, error })
}

fn parse_scope(text: &str, line: usize) -> Result<ShortcutScope, ShortcutProfileError> {
    if text == "global" {
        return Ok(ShortcutScope::Global);
    }
    let Some(local) = text.strip_prefix("local:") else {
        return Err(ShortcutProfileError::InvalidField {
            line,
            field: "scope",
        });
    };
    let local = decode_field(local).ok_or(ShortcutProfileError::InvalidField {
        line,
        field: "scope",
    })?;
    Ok(ShortcutScope::Local(local))
}

fn parse_chord(text: &str, line: usize) -> Result<Chord, ShortcutProfileError> {
    let invalid = || ShortcutProfileError::InvalidChord { line };
    let mut strokes = Vec::new();
    for stroke in text.split(',') {
        let Some((modifiers, key)) = stroke.split_once(':') else {
            return Err(invalid());
        };
        if key.contains(':') {
            return Err(invalid());
        }
        let modifiers = parse_modifiers(modifiers).ok_or_else(invalid)?;
        let key = decode_field(key).ok_or_else(invalid)?;
        strokes.push(KeyStroke::new(key, modifiers));
    }
    Chord::new(strokes).ok().ok_or_else(invalid)
}

fn parse_modifiers(text: &str) -> Option<Modifiers> {
    if text == "-" {
        return Some(Modifiers::default());
    }
    if text.is_empty() {
        return None;
    }
    let mut modifiers = Modifiers::default();
    let mut previous = 0u8;
    for character in text.chars() {
        let (rank, modifier) = match character {
            'c' => (1, Modifiers::CONTROL),
            'a' => (2, Modifiers::ALT),
            's' => (3, Modifiers::SHIFT),
            'm' => (4, Modifiers::META),
            _ => return None,
        };
        if rank <= previous {
            return None;
        }
        previous = rank;
        modifiers = modifiers | modifier;
    }
    Some(modifiers)
}

fn parse_target(
    text: &str,
    payload: &str,
    line: usize,
) -> Result<TemplateTarget, ShortcutProfileError> {
    let invalid = |field: &'static str| ShortcutProfileError::InvalidField { line, field };
    match text {
        "static" => {
            if !payload.is_empty() {
                return Err(invalid("target"));
            }
            Ok(TemplateTarget::Static)
        }
        "input" => {
            if let Some(slot) = payload.strip_prefix("slot:") {
                let slot = parse_canonical_u16(slot).ok_or_else(|| invalid("target"))?;
                Ok(TemplateTarget::Input(InputSelector::BySlot { slot }))
            } else if let Some(encoded) = payload.strip_prefix("input:") {
                let value = parse_canonical_u128(encoded).ok_or_else(|| invalid("target"))?;
                let value = NonZeroU128::new(value).ok_or_else(|| invalid("target"))?;
                Ok(TemplateTarget::Input(InputSelector::ByInput(InputId::new(
                    value,
                ))))
            } else {
                Err(invalid("target"))
            }
        }
        "value" => {
            let parts: Vec<&str> = payload.split(':').collect();
            let [stream, minimum, maximum] = parts.as_slice() else {
                return Err(invalid("target"));
            };
            let stream = decode_field(stream).ok_or_else(|| invalid("stream"))?;
            let minimum = parse_canonical_f64(minimum).ok_or_else(|| invalid("range"))?;
            let maximum = parse_canonical_f64(maximum).ok_or_else(|| invalid("range"))?;
            let range = ValueRange::new(minimum, maximum)
                .ok()
                .ok_or_else(|| invalid("range"))?;
            Ok(TemplateTarget::Value { stream, range })
        }
        _ => Err(invalid("target")),
    }
}

fn parse_canonical_u16(text: &str) -> Option<u16> {
    let value = text.parse::<u16>().ok()?;
    (value.to_string() == text).then_some(value)
}

fn parse_canonical_u128(text: &str) -> Option<u128> {
    let value = text.parse::<u128>().ok()?;
    (value.to_string() == text).then_some(value)
}

fn parse_canonical_f64(text: &str) -> Option<f64> {
    let value = text.parse::<f64>().ok()?;
    (value.to_string() == text).then_some(value)
}

fn render_template_line<C>(
    template: &ShortcutTemplate<C>,
    codec: &impl ShortcutProfileCodec<C>,
) -> String {
    let (delivery, stream) = match &template.intent {
        CommandIntent::Discrete(_) => ("discrete", String::new()),
        CommandIntent::Continuous { stream, .. } => ("continuous", escape_field(stream)),
        CommandIntent::CommitContinuous { stream, .. } => ("commit", escape_field(stream)),
    };
    let (target, payload) = match &template.target {
        TemplateTarget::Static => ("static", String::new()),
        TemplateTarget::Input(InputSelector::BySlot { slot }) => ("input", format!("slot:{slot}")),
        TemplateTarget::Input(InputSelector::ByInput(input)) => {
            ("input", format!("input:{}", input.get()))
        }
        TemplateTarget::Value { stream, range } => (
            "value",
            format!(
                "{}:{}:{}",
                escape_field(stream),
                range.minimum,
                range.maximum
            ),
        ),
    };
    let command = match &template.intent {
        CommandIntent::Discrete(command)
        | CommandIntent::Continuous { command, .. }
        | CommandIntent::CommitContinuous { command, .. } => command,
    };
    [
        escape_field(&template.id),
        escape_field(&template.name),
        render_scope(&template.scope),
        render_chord(&template.chord),
        delivery.to_owned(),
        stream,
        target.to_owned(),
        payload,
        codec.encode_command(command),
    ]
    .join("\t")
}

fn render_scope(scope: &ShortcutScope) -> String {
    match scope {
        ShortcutScope::Global => "global".to_owned(),
        ShortcutScope::Local(local) => format!("local:{}", escape_field(local)),
    }
}

fn render_chord(chord: &Chord) -> String {
    let mut text = String::new();
    for (index, stroke) in chord.strokes().iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push_str(&render_modifiers(stroke.modifiers));
        text.push(':');
        text.push_str(&escape_field(&stroke.key));
    }
    text
}

fn render_modifiers(modifiers: Modifiers) -> String {
    let mut text = String::new();
    if modifiers.contains(Modifiers::CONTROL) {
        text.push('c');
    }
    if modifiers.contains(Modifiers::ALT) {
        text.push('a');
    }
    if modifiers.contains(Modifiers::SHIFT) {
        text.push('s');
    }
    if modifiers.contains(Modifiers::META) {
        text.push('m');
    }
    if text.is_empty() {
        text.push('-');
    }
    text
}

fn escape_field(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '%' => escaped.push_str("%25"),
            '\t' => escaped.push_str("%09"),
            '\n' => escaped.push_str("%0A"),
            '\r' => escaped.push_str("%0D"),
            ',' => escaped.push_str("%2C"),
            ':' => escaped.push_str("%3A"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn unescape_field(text: &str) -> Option<String> {
    let mut unescaped = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            unescaped.push(character);
            continue;
        }
        let high = characters.next()?;
        let low = characters.next()?;
        let escaped = match (high, low) {
            ('0', '9') => '\t',
            ('0', 'A') => '\n',
            ('0', 'D') => '\r',
            ('2', '5') => '%',
            ('2', 'C') => ',',
            ('3', 'A') => ':',
            _ => return None,
        };
        unescaped.push(escaped);
    }
    Some(unescaped)
}

fn decode_field(text: &str) -> Option<String> {
    let decoded = unescape_field(text)?;
    (escape_field(&decoded) == text).then_some(decoded)
}

fn contains_line_break(text: &str) -> bool {
    text.contains(['\t', '\n', '\r'])
}

fn validate_stream(stream: &str) -> Result<(), TemplateError> {
    if stream.is_empty() {
        return Err(TemplateError::EmptyStream);
    }
    if stream.len() > MAX_STREAM_NAME_BYTES {
        return Err(TemplateError::StreamTooLong {
            maximum: MAX_STREAM_NAME_BYTES,
        });
    }
    if contains_line_break(stream) {
        return Err(TemplateError::InvalidText { field: "stream" });
    }
    Ok(())
}

fn template_scope_active(scope: &ShortcutScope, active: &str) -> bool {
    match scope {
        ShortcutScope::Global => true,
        ShortcutScope::Local(local) => local == active,
    }
}

fn resolve_target(
    target: &TemplateTarget,
    inputs: &[InputId],
    value: Option<f64>,
) -> Result<(Option<InputId>, Option<f64>), TemplateError> {
    match target {
        TemplateTarget::Static => {
            if !inputs.is_empty() {
                return Err(TemplateError::UnexpectedInput);
            }
            if value.is_some() {
                return Err(TemplateError::UnexpectedValue);
            }
            Ok((None, None))
        }
        TemplateTarget::Input(selector) => {
            if value.is_some() {
                return Err(TemplateError::UnexpectedValue);
            }
            let input = match selector {
                InputSelector::BySlot { slot } => {
                    let slot = *slot;
                    if slot == 0 {
                        return Err(TemplateError::InvalidSlot { slot });
                    }
                    if inputs.is_empty() {
                        return Err(TemplateError::EmptyInputRoster);
                    }
                    *inputs
                        .get(usize::from(slot - 1))
                        .ok_or(TemplateError::SlotOutOfRange {
                            slot,
                            roster_len: inputs.len(),
                        })?
                }
                InputSelector::ByInput(input) => *input,
            };
            Ok((Some(input), None))
        }
        TemplateTarget::Value { range, .. } => {
            let value = value.ok_or(TemplateError::MissingValue)?;
            if !value.is_finite() {
                return Err(TemplateError::NonFiniteValue);
            }
            if value < range.minimum || value > range.maximum {
                return Err(TemplateError::ValueOutOfRange {
                    value,
                    minimum: range.minimum,
                    maximum: range.maximum,
                });
            }
            Ok((None, Some(value)))
        }
    }
}
