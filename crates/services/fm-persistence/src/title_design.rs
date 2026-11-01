//! Stored title-designer documents (`.fmdesign`, GX-002).
//!
//! A design document is a strict, schema-tagged JSON file holding one
//! [`TitleDesign`]: its canvas, fields, node tree (elements and groups with
//! transforms and masks), roots and child order, and its In/Out animation
//! clip. Element and animation records reuse the `.fmtitle` codecs, so an
//! element or keyframe has one canonical form everywhere. Documents are
//! capped at [`MAX_TITLE_DESIGN_BYTES`].

use std::{
    collections::BTreeMap,
    error::Error,
    fmt::{self, Write as _},
    fs, io,
    num::NonZeroU128,
    path::Path,
};

use fm_titles::{
    Bounds, ClipMask, Color, DesignError, DesignNode, ElementId, FieldDefinition, Group,
    GroupTransform, TemplateId, TitleAnimation, TitleDesign,
};

use crate::{
    CURRENT_SCHEMA_VERSION,
    json::{Object, Reader, Value, parse_hex_color, syntax},
    title_template::{
        TitleTemplateError, parse_animation, parse_element, parse_field, write_color,
        write_element, write_field_value, write_string,
    },
};

/// Contract tag written into every design document.
pub const TITLE_DESIGN_CONTRACT: &str = "freemix.title-design";
/// Largest design document accepted, in bytes.
pub const MAX_TITLE_DESIGN_BYTES: u64 = 2 * 1024 * 1024;

/// Why a title-design document could not be read, written, or validated.
#[derive(Debug)]
pub enum TitleDesignError {
    /// The document exceeds [`MAX_TITLE_DESIGN_BYTES`].
    TooLarge { bytes: u64, maximum: u64 },
    /// The contract tag is not [`TITLE_DESIGN_CONTRACT`].
    Contract { found: String },
    /// The document's schema version is not the current one.
    Schema { found: u64, expected: u32 },
    /// The JSON syntax or a record is malformed.
    Decode(String),
    /// An element or animation record is invalid.
    Template(TitleTemplateError),
    /// The tree is not a valid design document.
    Design(DesignError),
    /// The file could not be read or written.
    Io(io::Error),
}

impl fmt::Display for TitleDesignError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { bytes, maximum } => {
                write!(
                    formatter,
                    "title design is {bytes} bytes, maximum is {maximum}"
                )
            }
            Self::Contract { found } => {
                write!(
                    formatter,
                    "title design contract {found:?} is not {TITLE_DESIGN_CONTRACT:?}"
                )
            }
            Self::Schema { found, expected } => {
                write!(formatter, "title design schema {found} is not {expected}")
            }
            Self::Decode(message) => write!(formatter, "title design JSON: {message}"),
            Self::Template(error) => error.fmt(formatter),
            Self::Design(error) => error.fmt(formatter),
            Self::Io(error) => write!(formatter, "title design I/O failed: {error}"),
        }
    }
}

impl Error for TitleDesignError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Template(error) => Some(error),
            Self::Design(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Decode(_)
            | Self::TooLarge { .. }
            | Self::Contract { .. }
            | Self::Schema { .. } => None,
        }
    }
}

impl From<crate::json::DecodeError> for TitleDesignError {
    fn from(error: crate::json::DecodeError) -> Self {
        Self::Decode(format!("{error:?}"))
    }
}

impl From<TitleTemplateError> for TitleDesignError {
    fn from(error: TitleTemplateError) -> Self {
        Self::Template(error)
    }
}

impl From<DesignError> for TitleDesignError {
    fn from(error: DesignError) -> Self {
        Self::Design(error)
    }
}

/// Encodes one design as a canonical `.fmdesign` document.
///
/// # Errors
///
/// Returns [`TitleDesignError::Design`] when the design is invalid, a template
/// error for an invalid element or animation record, or
/// [`TitleDesignError::TooLarge`] above [`MAX_TITLE_DESIGN_BYTES`].
pub fn encode_title_design(design: &TitleDesign) -> Result<String, TitleDesignError> {
    design.validate()?;
    let mut output = String::new();
    write!(
        output,
        "{{\n  \"contract\": \"{TITLE_DESIGN_CONTRACT}\",\n  \"schema_version\": {CURRENT_SCHEMA_VERSION},\n  \"id\": {},\n  \"name\": ",
        design.id()
    )
    .expect("writing to a string cannot fail");
    write_string(&mut output, design.name());
    write!(
        output,
        ",\n  \"width\": {},\n  \"height\": {},\n  \"background\": ",
        design.width(),
        design.height()
    )
    .expect("writing to a string cannot fail");
    write_color(&mut output, design.background());
    output.push_str(",\n  \"fields\": [");
    for (index, field) in design.fields().iter().enumerate() {
        output.push_str(if index == 0 { "\n    " } else { ",\n    " });
        write_field(&mut output, field);
    }
    output.push_str("],\n  \"nodes\": [");
    for (index, (_, node)) in design.nodes().enumerate() {
        output.push_str(if index == 0 { "\n    " } else { ",\n    " });
        match node {
            DesignNode::Element(element) => {
                output.push_str("{\"type\": \"element\", \"element\": ");
                write_element(&mut output, element);
                output.push('}');
            }
            DesignNode::Group(group) => {
                output.push_str("{\"type\": \"group\", \"group\": ");
                write_group(&mut output, group);
                output.push('}');
            }
        }
    }
    output.push_str("],\n  \"roots\": [");
    for (index, root) in design.roots().iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(output, "{root}").expect("writing to a string cannot fail");
    }
    output.push_str("],\n  \"children\": [");
    for (index, (group, children)) in design.children_map().iter().enumerate() {
        output.push_str(if index == 0 { "\n    " } else { ",\n    " });
        write!(output, "{{\"group\": {group}, \"children\": [")
            .expect("writing to a string cannot fail");
        for (child_index, child) in children.iter().enumerate() {
            if child_index != 0 {
                output.push_str(", ");
            }
            write!(output, "{child}").expect("writing to a string cannot fail");
        }
        output.push_str("]}");
    }
    output.push_str("],\n");
    let animation = TitleAnimation::new(design.animation().clone(), 0);
    crate::title_template::write_animation(&mut output, &animation)?;
    output.push_str("\n}\n");
    let bytes = u64::try_from(output.len()).unwrap_or(u64::MAX);
    if bytes > MAX_TITLE_DESIGN_BYTES {
        return Err(TitleDesignError::TooLarge {
            bytes,
            maximum: MAX_TITLE_DESIGN_BYTES,
        });
    }
    Ok(output)
}

/// Decodes one canonical `.fmdesign` document.
///
/// # Errors
///
/// Returns [`TitleDesignError`] for an oversized, malformed, or out-of-
/// contract document without accepting a partial design.
pub fn decode_title_design(source: &str) -> Result<TitleDesign, TitleDesignError> {
    let bytes = u64::try_from(source.len()).unwrap_or(u64::MAX);
    if bytes > MAX_TITLE_DESIGN_BYTES {
        return Err(TitleDesignError::TooLarge {
            bytes,
            maximum: MAX_TITLE_DESIGN_BYTES,
        });
    }
    let document = Reader::new(source).document().map_err(|error| {
        TitleDesignError::Decode(format!(
            "syntax error at byte {}: {}",
            error.offset, error.message
        ))
    })?;
    let mut root = Object::new(document, "title design")?;
    let contract = root.string("contract")?;
    if contract != TITLE_DESIGN_CONTRACT {
        return Err(TitleDesignError::Contract { found: contract });
    }
    let schema = root.u64("schema_version")?;
    if schema != u64::from(CURRENT_SCHEMA_VERSION) {
        return Err(TitleDesignError::Schema {
            found: schema,
            expected: CURRENT_SCHEMA_VERSION,
        });
    }
    let id = TemplateId::new(root.nonzero_u128("id")?);
    let name = root.string("name")?;
    let width = root.u32("width")?;
    let height = root.u32("height")?;
    let background = {
        let color = parse_hex_color(&root.string("background")?)?;
        Color::new(color.red, color.green, color.blue, color.alpha)
    };
    let fields = parse_fields(root.take("fields")?)?;
    let nodes = parse_nodes(root.take("nodes")?)?;
    let roots = parse_ids(root.take("roots")?)?;
    let children = parse_children(root.take("children")?)?;
    let animation = parse_animation(root.take("animation")?)?;
    root.finish()?;
    Ok(TitleDesign::from_parts(
        id,
        name,
        width,
        height,
        background,
        fields,
        nodes,
        roots,
        children,
        animation.clip,
    )?)
}

/// Writes `design` to `path` as a canonical document.
///
/// # Errors
///
/// Returns the encode errors or the file write error.
pub fn write_title_design_file(path: &Path, design: &TitleDesign) -> Result<(), TitleDesignError> {
    let document = encode_title_design(design)?;
    fs::write(path, document).map_err(TitleDesignError::Io)
}

/// Reads and validates one design document from `path`.
///
/// # Errors
///
/// Returns [`TitleDesignError::TooLarge`] above [`MAX_TITLE_DESIGN_BYTES`],
/// the decode/validation error, or the file read error.
pub fn read_title_design_file(path: &Path) -> Result<TitleDesign, TitleDesignError> {
    let metadata = fs::metadata(path).map_err(TitleDesignError::Io)?;
    if metadata.len() > MAX_TITLE_DESIGN_BYTES {
        return Err(TitleDesignError::TooLarge {
            bytes: metadata.len(),
            maximum: MAX_TITLE_DESIGN_BYTES,
        });
    }
    let source = fs::read_to_string(path).map_err(TitleDesignError::Io)?;
    decode_title_design(&source)
}

fn write_field(output: &mut String, field: &FieldDefinition) {
    write!(output, "{{\"id\": {}, \"name\": ", field.id).expect("writing cannot fail");
    write_string(output, &field.name);
    output.push_str(", \"default\": ");
    write_field_value(output, &field.default);
    if !field.exposed {
        output.push_str(", \"exposed\": false");
    }
    output.push('}');
}

fn write_group(output: &mut String, group: &Group) {
    write!(output, "{{\"id\": {}, \"name\": ", group.id).expect("writing cannot fail");
    write_string(output, &group.name);
    write!(
        output,
        ", \"visible\": {}, \"offset_x\": {}, \"offset_y\": {}, \"opacity\": {}, \"mask\": ",
        group.visible, group.transform.offset_x, group.transform.offset_y, group.transform.opacity
    )
    .expect("writing cannot fail");
    match &group.mask {
        None => output.push_str("null"),
        Some(mask) => {
            write!(
                output,
                "{{\"x\": {}, \"y\": {}, \"width\": {}, \"height\": {}, \"corner_radius\": {}}}",
                mask.bounds.x,
                mask.bounds.y,
                mask.bounds.width,
                mask.bounds.height,
                mask.corner_radius
            )
            .expect("writing cannot fail");
        }
    }
    output.push('}');
}

fn parse_fields(value: Value) -> Result<Vec<FieldDefinition>, TitleDesignError> {
    let Value::Array(values) = value else {
        return Err(syntax("title design fields must be an array").into());
    };
    values
        .into_iter()
        .map(parse_field)
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

fn parse_nodes(value: Value) -> Result<BTreeMap<ElementId, DesignNode>, TitleDesignError> {
    let Value::Array(values) = value else {
        return Err(syntax("title design nodes must be an array").into());
    };
    let mut nodes = BTreeMap::new();
    for value in values {
        let mut object = Object::new(value, "title design node")?;
        let node = match object.string("type")?.as_str() {
            "element" => DesignNode::Element(parse_element(object.take("element")?)?),
            "group" => DesignNode::Group(parse_group(object.take("group")?)?),
            kind => return Err(syntax(format!("unknown title design node type {kind:?}")).into()),
        };
        object.finish()?;
        let id = node.id();
        if nodes.insert(id, node).is_some() {
            return Err(DesignError::DuplicateNode(id).into());
        }
    }
    Ok(nodes)
}

fn parse_group(value: Value) -> Result<Group, TitleDesignError> {
    let mut object = Object::new(value, "title design group")?;
    let group = Group {
        id: ElementId::new(object.nonzero_u128("id")?),
        name: object.string("name")?,
        visible: object.boolean("visible")?,
        transform: GroupTransform {
            offset_x: object.i32("offset_x")?,
            offset_y: object.i32("offset_y")?,
            opacity: object.u8("opacity")?,
        },
        mask: match object.take("mask")? {
            Value::Null => None,
            value => Some(parse_mask(value)?),
        },
    };
    object.finish()?;
    Ok(group)
}

fn parse_mask(value: Value) -> Result<ClipMask, TitleDesignError> {
    let mut object = Object::new(value, "title design mask")?;
    let mask = ClipMask {
        bounds: Bounds {
            x: object.i32("x")?,
            y: object.i32("y")?,
            width: object.u32("width")?,
            height: object.u32("height")?,
        },
        corner_radius: object.u32("corner_radius")?,
    };
    object.finish()?;
    Ok(mask)
}

fn parse_ids(value: Value) -> Result<Vec<ElementId>, TitleDesignError> {
    let Value::Array(values) = value else {
        return Err(syntax("title design roots must be an array").into());
    };
    values.iter().map(parse_id).collect()
}

fn parse_id(value: &Value) -> Result<ElementId, TitleDesignError> {
    let Value::Number(value) = value else {
        return Err(syntax("a title design id must be a positive integer").into());
    };
    let value =
        NonZeroU128::new(*value).ok_or_else(|| syntax("a title design id must not be zero"))?;
    Ok(ElementId::new(value))
}

fn parse_children(value: Value) -> Result<BTreeMap<ElementId, Vec<ElementId>>, TitleDesignError> {
    let Value::Array(values) = value else {
        return Err(syntax("title design children must be an array").into());
    };
    let mut children = BTreeMap::new();
    for value in values {
        let mut object = Object::new(value, "title design children")?;
        let group = ElementId::new(object.nonzero_u128("group")?);
        let ids = parse_ids(object.take("children")?)?;
        object.finish()?;
        if children.insert(group, ids).is_some() {
            return Err(syntax("a title design group appears twice in children").into());
        }
    }
    Ok(children)
}
