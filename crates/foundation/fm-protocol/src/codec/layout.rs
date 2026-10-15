//! Title template layouts (Protocol 2.30).
//!
//! A layout's canvas travels as the plain `width` and `height` fields; its
//! element list travels as one unpadded base64url `elements` field holding a
//! compact binary encoding with the varints of the clip codec (minimal
//! unsigned LEB128, signed values zigzag-mapped):
//!
//! ```text
//! elements = count element*
//! element  = id(varint, nonzero u128) kind(byte: index in
//!            WireTitleElementKind::ALL) flags(byte: bit 0 visible, other
//!            bits zero) name_len(varint) name(UTF-8 bytes)
//!            x(zigzag) y(zigzag) width(varint) height(varint)
//!            opacity(byte) r g b a(bytes)
//! ```
//!
//! Elements keep template order, so every layout has exactly one encoding.
//! Both directions enforce: the canvas is both zero (no template, and then
//! no elements) or both sides in `1..=MAX_TITLE_CANVAS_SIDE`; at most
//! [`MAX_TITLE_LAYOUT_ELEMENTS`] elements with unique ids; names at most
//! [`MAX_TITLE_ELEMENT_NAME_BYTES`] bytes of UTF-8 without control
//! characters; and the encoding at most [`MAX_TITLE_LAYOUT_BYTES`].

use core::num::NonZeroU128;
use std::collections::BTreeSet;

use fm_animation::Rgba;

use super::animation::{Reader, push_varint, zigzag};
use crate::{
    CodecError, MAX_TITLE_CANVAS_SIDE, MAX_TITLE_ELEMENT_NAME_BYTES, MAX_TITLE_LAYOUT_BYTES,
    MAX_TITLE_LAYOUT_ELEMENTS, WireTitleElementId, WireTitleElementKind, WireTitleElementOutline,
    WireTitleLayout,
};

const ELEMENTS: &str = "elements";
const VISIBLE: u8 = 1;

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn kind_tag(kind: WireTitleElementKind) -> u8 {
    let index = WireTitleElementKind::ALL
        .iter()
        .position(|candidate| *candidate == kind)
        .unwrap_or_default();
    u8::try_from(index).unwrap_or_default()
}

/// Whether `name` is a valid element display name.
fn valid_name(name: &str) -> bool {
    name.len() <= MAX_TITLE_ELEMENT_NAME_BYTES && !name.chars().any(char::is_control)
}

/// Enforces every layout bound except the encoded size.
fn validate_layout(layout: &WireTitleLayout) -> Result<(), CodecError> {
    let side = |field: &'static str, value: u32| {
        if value <= MAX_TITLE_CANVAS_SIDE {
            Ok(())
        } else {
            Err(invalid(field, value.to_string()))
        }
    };
    side("width", layout.width)?;
    side("height", layout.height)?;
    if (layout.width == 0) != (layout.height == 0) {
        return Err(invalid(
            "height",
            format!("{}x{}", layout.width, layout.height),
        ));
    }
    if layout.width == 0 && !layout.elements.is_empty() {
        return Err(invalid(
            ELEMENTS,
            format!("{} elements without a canvas", layout.elements.len()),
        ));
    }
    if layout.elements.len() > MAX_TITLE_LAYOUT_ELEMENTS {
        return Err(invalid(
            ELEMENTS,
            format!("{} elements", layout.elements.len()),
        ));
    }
    let mut ids = BTreeSet::new();
    for element in &layout.elements {
        if !ids.insert(element.id) {
            return Err(invalid(
                ELEMENTS,
                format!("duplicate element {}", element.id),
            ));
        }
        if !valid_name(&element.name) {
            return Err(invalid(
                ELEMENTS,
                format!(
                    "element {} name of {} bytes",
                    element.id,
                    element.name.len()
                ),
            ));
        }
    }
    Ok(())
}

fn elements_bytes(layout: &WireTitleLayout) -> Vec<u8> {
    let mut out = Vec::new();
    push_varint(&mut out, layout.elements.len() as u128);
    for element in &layout.elements {
        push_varint(&mut out, element.id.get().get());
        out.push(kind_tag(element.kind));
        out.push(if element.visible { VISIBLE } else { 0 });
        push_varint(&mut out, element.name.len() as u128);
        out.extend_from_slice(element.name.as_bytes());
        push_varint(&mut out, u128::from(zigzag(i64::from(element.x))));
        push_varint(&mut out, u128::from(zigzag(i64::from(element.y))));
        push_varint(&mut out, u128::from(element.width));
        push_varint(&mut out, u128::from(element.height));
        out.push(element.opacity);
        let fill = element.fill;
        out.extend_from_slice(&[fill.r, fill.g, fill.b, fill.a]);
    }
    out
}

/// Validates `layout` and encodes its element list as one base64url field
/// value.
///
/// # Errors
///
/// [`CodecError::InvalidField`] naming `width`, `height`, or `elements` for
/// a layout outside the bounds of the module documentation.
pub(super) fn encode_layout_elements(layout: &WireTitleLayout) -> Result<String, CodecError> {
    validate_layout(layout)?;
    let bytes = elements_bytes(layout);
    if bytes.len() > MAX_TITLE_LAYOUT_BYTES {
        return Err(invalid(ELEMENTS, format!("{} bytes", bytes.len())));
    }
    Ok(crate::monitor_image::base64url_encode(&bytes))
}

impl Reader<'_> {
    fn element(&mut self) -> Option<WireTitleElementOutline> {
        let id = WireTitleElementId::new(NonZeroU128::new(self.varint(128)?)?);
        let kind = *WireTitleElementKind::ALL.get(usize::from(self.byte()?))?;
        let visible = match self.byte()? {
            0 => false,
            VISIBLE => true,
            _ => return None,
        };
        let length = usize::try_from(self.varint(32)?).ok()?;
        if length > MAX_TITLE_ELEMENT_NAME_BYTES {
            return None;
        }
        let end = self.position.checked_add(length)?;
        let name = core::str::from_utf8(self.bytes.get(self.position..end)?)
            .ok()?
            .to_owned();
        self.position = end;
        let x = self.i32()?;
        let y = self.i32()?;
        let width = self.u32()?;
        let height = self.u32()?;
        let opacity = self.byte()?;
        let [red, green, blue, alpha] = [self.byte()?, self.byte()?, self.byte()?, self.byte()?];
        Some(WireTitleElementOutline {
            id,
            name,
            kind,
            x,
            y,
            width,
            height,
            opacity,
            fill: Rgba::new(red, green, blue, alpha),
            visible,
        })
    }

    fn elements(&mut self) -> Option<Vec<WireTitleElementOutline>> {
        let count = usize::try_from(self.varint(32)?).ok()?;
        if count > MAX_TITLE_LAYOUT_ELEMENTS {
            return None;
        }
        let mut elements = Vec::with_capacity(count);
        for _ in 0..count {
            elements.push(self.element()?);
        }
        (self.position == self.bytes.len()).then_some(elements)
    }
}

/// Strictly decodes a layout from its canvas fields and base64url element
/// list.
///
/// # Errors
///
/// [`CodecError::InvalidField`] naming `elements` for non-canonical
/// base64url or an oversized, truncated, non-minimal, or trailing encoding,
/// and naming the offending field for any bound of the module
/// documentation.
pub(super) fn decode_layout(
    width: u32,
    height: u32,
    elements: &str,
) -> Result<WireTitleLayout, CodecError> {
    let encoded_limit = (MAX_TITLE_LAYOUT_BYTES * 4).div_ceil(3);
    if elements.len() > encoded_limit {
        return Err(invalid(ELEMENTS, format!("{} characters", elements.len())));
    }
    let bytes = crate::monitor_image::base64url_decode(elements)
        .ok_or_else(|| invalid(ELEMENTS, format!("{} characters", elements.len())))?;
    let elements = Reader {
        bytes: &bytes,
        position: 0,
    }
    .elements()
    .ok_or_else(|| invalid(ELEMENTS, format!("{} bytes", bytes.len())))?;
    let layout = WireTitleLayout {
        width,
        height,
        elements,
    };
    validate_layout(&layout)?;
    Ok(layout)
}
