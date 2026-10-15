//! Scene layouts (Protocol 2.31).
//!
//! A layout's canvas travels as the plain `width` and `height` fields; its
//! layer list travels as one unpadded base64url `layers` field holding a
//! compact binary encoding with the varints of the clip codec (minimal
//! unsigned LEB128, signed values zigzag-mapped):
//!
//! ```text
//! layers = count layer*
//! layer  = id(varint, nonzero u32) flags(byte: bit 0 enabled, other bits
//!          zero) quarter_turns(byte, 0..=3) name_len(varint)
//!          name(UTF-8 bytes) x(zigzag) y(zigzag) width(varint)
//!          height(varint) opacity(byte)
//! ```
//!
//! Layers keep scene order, so every layout has exactly one encoding. Both
//! directions enforce: both canvas sides in `1..=MAX_SCENE_CANVAS_SIDE`; at
//! most [`MAX_SCENE_LAYOUT_LAYERS`] layers with unique ids; names at most
//! [`MAX_SCENE_LAYER_NAME_BYTES`] bytes of UTF-8 without control characters;
//! and the encoding at most [`MAX_SCENE_LAYOUT_BYTES`].

use core::num::NonZeroU32;
use std::collections::BTreeSet;

use super::animation::{Reader, push_varint, zigzag};
use crate::{
    CodecError, MAX_SCENE_CANVAS_SIDE, MAX_SCENE_LAYER_NAME_BYTES, MAX_SCENE_LAYOUT_BYTES,
    MAX_SCENE_LAYOUT_LAYERS, WireInputId, WireSceneClip, WireSceneLayerId, WireSceneLayerOutline,
    WireSceneLayout,
};

const LAYERS: &str = "layers";
const ENABLED: u8 = 1;

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

/// Enforces every layout bound except the encoded size.
fn validate_layout(layout: &WireSceneLayout) -> Result<(), CodecError> {
    let side = |field: &'static str, value: u32| {
        if (1..=MAX_SCENE_CANVAS_SIDE).contains(&value) {
            Ok(())
        } else {
            Err(invalid(field, value.to_string()))
        }
    };
    side("width", layout.width)?;
    side("height", layout.height)?;
    if layout.layers.len() > MAX_SCENE_LAYOUT_LAYERS {
        return Err(invalid(LAYERS, format!("{} layers", layout.layers.len())));
    }
    let mut ids = BTreeSet::new();
    for layer in &layout.layers {
        if !ids.insert(layer.id) {
            return Err(invalid(LAYERS, format!("duplicate layer {}", layer.id)));
        }
        if layer.name.len() > MAX_SCENE_LAYER_NAME_BYTES || layer.name.chars().any(char::is_control)
        {
            return Err(invalid(
                LAYERS,
                format!("layer {} name of {} bytes", layer.id, layer.name.len()),
            ));
        }
        if layer.quarter_turns > 3 {
            return Err(invalid(
                LAYERS,
                format!("layer {} turned {} quarters", layer.id, layer.quarter_turns),
            ));
        }
    }
    Ok(())
}

fn layers_bytes(layout: &WireSceneLayout) -> Vec<u8> {
    let mut out = Vec::new();
    push_varint(&mut out, layout.layers.len() as u128);
    for layer in &layout.layers {
        push_varint(&mut out, u128::from(layer.id.get().get()));
        out.push(if layer.enabled { ENABLED } else { 0 });
        out.push(layer.quarter_turns);
        push_varint(&mut out, layer.name.len() as u128);
        out.extend_from_slice(layer.name.as_bytes());
        push_varint(&mut out, u128::from(zigzag(i64::from(layer.x))));
        push_varint(&mut out, u128::from(zigzag(i64::from(layer.y))));
        push_varint(&mut out, u128::from(layer.width));
        push_varint(&mut out, u128::from(layer.height));
        out.push(layer.opacity);
    }
    out
}

/// Validates `layout` and encodes its layer list as one base64url field
/// value.
///
/// # Errors
///
/// [`CodecError::InvalidField`] naming `width`, `height`, or `layers` for a
/// layout outside the bounds of the module documentation.
pub(super) fn encode_scene_layers(layout: &WireSceneLayout) -> Result<String, CodecError> {
    validate_layout(layout)?;
    let bytes = layers_bytes(layout);
    if bytes.len() > MAX_SCENE_LAYOUT_BYTES {
        return Err(invalid(LAYERS, format!("{} bytes", bytes.len())));
    }
    Ok(crate::monitor_image::base64url_encode(&bytes))
}

impl Reader<'_> {
    fn scene_layer(&mut self) -> Option<WireSceneLayerOutline> {
        let id = WireSceneLayerId::new(NonZeroU32::new(self.u32()?)?);
        let enabled = match self.byte()? {
            0 => false,
            ENABLED => true,
            _ => return None,
        };
        let quarter_turns = self.byte()?;
        let length = usize::try_from(self.varint(32)?).ok()?;
        if length > MAX_SCENE_LAYER_NAME_BYTES {
            return None;
        }
        let end = self.position.checked_add(length)?;
        let name = core::str::from_utf8(self.bytes.get(self.position..end)?)
            .ok()?
            .to_owned();
        self.position = end;
        Some(WireSceneLayerOutline {
            id,
            name,
            x: self.i32()?,
            y: self.i32()?,
            width: self.u32()?,
            height: self.u32()?,
            quarter_turns,
            opacity: self.byte()?,
            enabled,
        })
    }

    fn scene_layers(&mut self) -> Option<Vec<WireSceneLayerOutline>> {
        let count = usize::try_from(self.varint(32)?).ok()?;
        if count > MAX_SCENE_LAYOUT_LAYERS {
            return None;
        }
        let mut layers = Vec::with_capacity(count);
        for _ in 0..count {
            layers.push(self.scene_layer()?);
        }
        (self.position == self.bytes.len()).then_some(layers)
    }
}

/// Strictly decodes a scene layout from its canvas fields and base64url
/// layer list.
///
/// # Errors
///
/// [`CodecError::InvalidField`] naming `layers` for non-canonical base64url
/// or an oversized, truncated, non-minimal, or trailing encoding, and naming
/// the offending field for any bound of the module documentation.
pub(super) fn decode_scene_layout(
    width: u32,
    height: u32,
    layers: &str,
) -> Result<WireSceneLayout, CodecError> {
    let encoded_limit = (MAX_SCENE_LAYOUT_BYTES * 4).div_ceil(3);
    if layers.len() > encoded_limit {
        return Err(invalid(LAYERS, format!("{} characters", layers.len())));
    }
    let bytes = crate::monitor_image::base64url_decode(layers)
        .ok_or_else(|| invalid(LAYERS, format!("{} characters", layers.len())))?;
    let layers = Reader {
        bytes: &bytes,
        position: 0,
    }
    .scene_layers()
    .ok_or_else(|| invalid(LAYERS, format!("{} bytes", bytes.len())))?;
    let layout = WireSceneLayout {
        width,
        height,
        layers,
    };
    validate_layout(&layout)?;
    Ok(layout)
}

const SCENES: &str = "scenes";

/// Enforces a snapshot's `scenes` list: strictly ascending inputs of the
/// snapshot, at most [`super::MAX_LIST_ITEMS`].
fn validate_scene_list(scenes: &[WireInputId], inputs: &[WireInputId]) -> Result<(), CodecError> {
    if scenes.len() > super::MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems(SCENES));
    }
    let ascending = scenes.windows(2).all(|pair| pair[0].get() < pair[1].get());
    if !ascending || scenes.iter().any(|scene| !inputs.contains(scene)) {
        return Err(invalid(SCENES, join_inputs(scenes)));
    }
    Ok(())
}

fn join_inputs(scenes: &[WireInputId]) -> String {
    scenes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Encodes a snapshot's `scenes` list (possibly empty).
pub(super) fn encode_scene_list(
    scenes: &[WireInputId],
    inputs: &[WireInputId],
) -> Result<String, CodecError> {
    validate_scene_list(scenes, inputs)?;
    Ok(join_inputs(scenes))
}

/// Strictly decodes a snapshot's `scenes` list against its inputs.
pub(super) fn decode_scene_list(
    value: &str,
    inputs: &[WireInputId],
) -> Result<Vec<WireInputId>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').nth(super::MAX_LIST_ITEMS).is_some() {
        return Err(CodecError::TooManyItems(SCENES));
    }
    let scenes = value
        .split(',')
        .map(|entry| {
            super::value::parse_canonical_input(entry).ok_or_else(|| invalid(SCENES, entry))
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_scene_list(&scenes, inputs)?;
    Ok(scenes)
}

/// Refuses a scene clip animating colour, which scene layers do not have.
fn check_scene_clip(clip: &WireSceneClip) -> Result<(), CodecError> {
    if clip
        .tracks()
        .iter()
        .any(|track| track.key.property == fm_animation::Property::Color)
    {
        return Err(invalid("clip", "scene layers do not animate color"));
    }
    Ok(())
}

/// Encodes a scene clip in the shared clip encoding.
pub(super) fn encode_scene_clip(clip: &WireSceneClip) -> Result<String, CodecError> {
    check_scene_clip(clip)?;
    super::animation::encode_clip(clip)
}

/// Strictly decodes a scene clip.
pub(super) fn decode_scene_clip(value: &str) -> Result<WireSceneClip, CodecError> {
    let clip = super::animation::decode_clip(value)?;
    check_scene_clip(&clip)?;
    Ok(clip)
}
