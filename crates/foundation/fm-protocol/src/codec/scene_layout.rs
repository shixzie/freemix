//! Scene layouts (Protocol 2.31; layer sources 2.35; full layer design
//! 2.39; keys and effects 2.43; key/fill and key refinements 2.70).
//!
//! A layout's canvas travels as the plain `width` and `height` fields; its
//! layer list travels as one unpadded base64url `layers` field holding a
//! compact binary encoding with the varints of the clip codec (minimal
//! unsigned LEB128, signed values zigzag-mapped):
//!
//! ```text
//! layers = count layer*
//! layer  = id(varint, nonzero u32) flags(byte: bit 0 enabled, bit 1 crop,
//!          bit 2 mask, bit 3 mask inverted (only with bit 2), bit 4 key,
//!          other bits zero) quarter_turns(byte, 0..=3) name_len(varint)
//!          name(UTF-8 bytes) source x(zigzag) y(zigzag) width(varint)
//!          height(varint) opacity(byte) tilt(zigzag) anchor_x(varint)
//!          anchor_y(varint) z_order(zigzag) crop? mask? key?
//!          effect_count(varint, at most MAX_SCENE_LAYER_EFFECTS) effect*
//! source = kind(byte: 0 input, 1 scene) id(varint, nonzero u128)
//! crop   = x(varint) y(varint) width(varint) height(varint)
//! mask   = x(varint) y(varint) width(varint) height(varint)
//! key    = 0(byte) red green blue similarity smoothness spill (bytes) refine
//!        | 1(byte) low high softness (bytes) invert(byte 0 or 1) refine
//!        | 2(byte) key_input(varint, nonzero u128) channel(byte: 0 luma,
//!          1 alpha) low high softness (bytes) invert(byte 0 or 1) refine
//! refine = clip_black clip_white (bytes) choke(zigzag) edge_softness(byte)
//!          despill(byte: 0 off, 1 green, 2 blue) strength(byte, 0 when off)
//!          light_wrap light_wrap_width (bytes, 2.97; width 0 exactly when
//!          light_wrap is 0) matte(byte, 2.97: 0 none, else count << 1 |
//!          outside, count 3..=8) [points, unless none]
//! points = the count x, y pairs as 10-bit coordinates (each 0..=1000),
//!          packed least significant bit first into ceil(20 * count / 8)
//!          bytes whose spare high bits are zero
//! effect = kind(byte, index into SCENE_LAYER_EFFECT_KINDS)
//!          value(zigzag)* (exactly the kind's parameter count)
//! ```
//!
//! Layers keep scene order, so every layout has exactly one encoding. Both
//! directions enforce: both canvas sides in `1..=MAX_SCENE_CANVAS_SIDE`; at
//! most [`MAX_SCENE_LAYOUT_LAYERS`] layers with unique ids; names at most
//! [`MAX_SCENE_LAYER_NAME_BYTES`] bytes of UTF-8 without control characters;
//! tilts in `MIN_SCENE_LAYER_TILT_MILLIDEGREES..MAX_SCENE_LAYER_TILT_MILLIDEGREES`;
//! anchors within `0..=SCENE_LAYER_ANCHOR_MAX`; crops nonempty and inside
//! the canvas and masks nonempty and inside the cropped source; luma and
//! key/fill keys with `low < high` and every key refinement within its
//! bounds ([`crate::WireKeyRefinement::is_valid`]); every effect valid
//! ([`crate::WireLayerEffect::kind_index`]); and the encoding at most
//! [`MAX_SCENE_LAYOUT_BYTES`].
//!
//! A `scene_layer_add` carries one complete layer as its `spec` field: the
//! same `layer` encoding (base64url, unpadded). A `scene_layer_effects_set`
//! carries its list as its `effects` field: `effect_count effect*`
//! (base64url, unpadded; the empty list is `AA`).

use core::num::NonZeroU32;
use std::collections::BTreeSet;

use super::animation::{Reader, push_varint, zigzag};
use crate::{
    CodecError, MAX_SCENE_CANVAS_SIDE, MAX_SCENE_LAYER_EFFECTS, MAX_SCENE_LAYER_NAME_BYTES,
    MAX_SCENE_LAYER_TILT_MILLIDEGREES, MAX_SCENE_LAYOUT_BYTES, MAX_SCENE_LAYOUT_LAYERS,
    MIN_SCENE_LAYER_TILT_MILLIDEGREES, SCENE_LAYER_ANCHOR_MAX, SCENE_LAYER_EFFECT_KINDS,
    WireChromaKey, WireFillKey, WireInputId, WireKeyDespill, WireKeyRefinement, WireLayerCrop,
    WireLayerEffect, WireLayerKey, WireLayerMask, WireLumaKey, WireSceneClip, WireSceneId,
    WireSceneLayerId, WireSceneLayerOutline, WireSceneLayerSource, WireSceneLayout,
};

const LAYERS: &str = "layers";
const SPEC: &str = "spec";
const ENABLED: u8 = 1;
const CROPPED: u8 = 2;
const MASKED: u8 = 4;
const MASK_INVERTED: u8 = 8;
const KEYED: u8 = 16;
const KEY_CHROMA: u8 = 0;
const KEY_LUMA: u8 = 1;
const KEY_FILL: u8 = 2;
const DESPILL_OFF: u8 = 0;
const DESPILL_GREEN: u8 = 1;
const DESPILL_BLUE: u8 = 2;
const EFFECTS: &str = "effects";
const SOURCE_INPUT: u8 = 0;
const SOURCE_SCENE: u8 = 1;

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
        validate_design(LAYERS, layer, Some((layout.width, layout.height)))?;
    }
    Ok(())
}

/// Whether `x, y, width, height` is nonempty and inside `limit`.
const fn rect_inside(x: u32, y: u32, width: u32, height: u32, limit: (u32, u32)) -> bool {
    width != 0
        && height != 0
        && x as u64 + width as u64 <= limit.0 as u64
        && y as u64 + height as u64 <= limit.1 as u64
}

/// Enforces one layer's quarter turns, tilt, anchor, crop, and mask; crops
/// and masks are checked against `canvas` when it is known (a layout), and
/// only for being nonempty otherwise (a spec: the authority knows the
/// canvas).
fn validate_design(
    field: &'static str,
    layer: &WireSceneLayerOutline,
    canvas: Option<(u32, u32)>,
) -> Result<(), CodecError> {
    let refuse = |what: &str| Err(invalid(field, format!("layer {} {what}", layer.id)));
    if layer.quarter_turns > 3 {
        return refuse(&format!("turned {} quarters", layer.quarter_turns));
    }
    if !(MIN_SCENE_LAYER_TILT_MILLIDEGREES..MAX_SCENE_LAYER_TILT_MILLIDEGREES)
        .contains(&layer.tilt_millidegrees)
    {
        return refuse(&format!("tilted {}", layer.tilt_millidegrees));
    }
    if layer.anchor_x > SCENE_LAYER_ANCHOR_MAX || layer.anchor_y > SCENE_LAYER_ANCHOR_MAX {
        return refuse("anchored outside its box");
    }
    let unbounded = (u32::MAX, u32::MAX);
    if let Some(crop) = layer.crop
        && !rect_inside(
            crop.x,
            crop.y,
            crop.width,
            crop.height,
            canvas.unwrap_or(unbounded),
        )
    {
        return refuse("cropped outside its source");
    }
    if let Some(mask) = layer.mask {
        let source = match (layer.crop, canvas) {
            (Some(crop), _) => (crop.width, crop.height),
            (None, Some(canvas)) => canvas,
            (None, None) => unbounded,
        };
        if !rect_inside(mask.x, mask.y, mask.width, mask.height, source) {
            return refuse("masked outside its cropped source");
        }
    }
    if layer.key.is_some_and(|key| !key.is_valid()) {
        return refuse("keyed with an empty luma band or an invalid refinement");
    }
    if layer.effects.len() > MAX_SCENE_LAYER_EFFECTS {
        return refuse(&format!("carrying {} effects", layer.effects.len()));
    }
    if let Some(effect) = layer.effects.iter().find(|effect| !effect.is_valid()) {
        return refuse(&format!("with effect {:?}", effect.name));
    }
    Ok(())
}

/// Validates an effect list for `scene_layer_effects_set`.
fn validate_effect_list(effects: &[WireLayerEffect]) -> Result<(), CodecError> {
    if effects.len() > MAX_SCENE_LAYER_EFFECTS {
        return Err(invalid(EFFECTS, format!("{} effects", effects.len())));
    }
    if let Some(effect) = effects.iter().find(|effect| !effect.is_valid()) {
        return Err(invalid(EFFECTS, format!("effect {:?}", effect.name)));
    }
    Ok(())
}

fn push_effects(out: &mut Vec<u8>, effects: &[WireLayerEffect]) {
    push_varint(out, effects.len() as u128);
    for effect in effects {
        out.push(effect.kind_index().unwrap_or(u8::MAX));
        for value in &effect.parameters {
            push_varint(out, u128::from(zigzag(i64::from(*value))));
        }
    }
}

fn push_key(out: &mut Vec<u8>, key: WireLayerKey) {
    match key {
        WireLayerKey::Chroma(chroma) => out.extend_from_slice(&[
            KEY_CHROMA,
            chroma.red,
            chroma.green,
            chroma.blue,
            chroma.similarity,
            chroma.smoothness,
            chroma.spill,
        ]),
        WireLayerKey::Luma(luma) => out.extend_from_slice(&[
            KEY_LUMA,
            luma.low,
            luma.high,
            luma.softness,
            u8::from(luma.invert),
        ]),
        WireLayerKey::Fill(fill) => {
            out.push(KEY_FILL);
            push_varint(out, fill.key_input.get().get());
            out.extend_from_slice(&[
                u8::from(fill.alpha_channel),
                fill.low,
                fill.high,
                fill.softness,
                u8::from(fill.invert),
            ]);
        }
    }
    let refinement = key.refinement();
    out.extend_from_slice(&[refinement.clip_black, refinement.clip_white]);
    push_varint(out, u128::from(zigzag(i64::from(refinement.choke))));
    let (despill, strength) = match refinement.despill {
        WireKeyDespill::Off => (DESPILL_OFF, 0),
        WireKeyDespill::Green { strength } => (DESPILL_GREEN, strength),
        WireKeyDespill::Blue { strength } => (DESPILL_BLUE, strength),
    };
    out.extend_from_slice(&[
        refinement.edge_softness,
        despill,
        strength,
        refinement.light_wrap,
        refinement.light_wrap_width,
    ]);
    match refinement.garbage_matte {
        None => out.push(0),
        Some(matte) => {
            let count = u8::try_from(matte.points().len()).unwrap_or(0);
            out.push(count << 1 | u8::from(matte.invert()));
            let mut bits = 0_u32;
            let mut pending = 0_u32;
            for point in matte.points() {
                for coordinate in [point.x, point.y] {
                    bits |= u32::from(coordinate & MATTE_COORDINATE_MASK) << pending;
                    pending += MATTE_COORDINATE_BITS;
                    while pending >= 8 {
                        out.push(bits.to_le_bytes()[0]);
                        bits >>= 8;
                        pending -= 8;
                    }
                }
            }
            if pending > 0 {
                out.push(bits.to_le_bytes()[0]);
            }
        }
    }
}

/// Bits of one packed garbage matte coordinate (`0..=1000`).
const MATTE_COORDINATE_BITS: u32 = 10;
const MATTE_COORDINATE_MASK: u16 = (1 << MATTE_COORDINATE_BITS) - 1;
const _: () = assert!(fm_types::GARBAGE_MATTE_UNIT <= MATTE_COORDINATE_MASK);

/// Validates and encodes a `scene_layer_effects_set` list.
///
/// # Errors
///
/// [`CodecError::InvalidField`] naming `effects` for a list outside the
/// bounds of the module documentation.
pub(super) fn encode_effect_list(effects: &[WireLayerEffect]) -> Result<String, CodecError> {
    validate_effect_list(effects)?;
    let mut out = Vec::new();
    push_effects(&mut out, effects);
    Ok(crate::monitor_image::base64url_encode(&out))
}

/// Strictly decodes a `scene_layer_effects_set` list.
///
/// # Errors
///
/// [`CodecError::InvalidField`] naming `effects` for non-canonical
/// base64url, a truncated or trailing encoding, or any effect bound.
pub(super) fn decode_effect_list(value: &str) -> Result<Vec<WireLayerEffect>, CodecError> {
    // At most 8 effects of a kind byte and two 5-byte values each.
    if value.len() > 128 {
        return Err(invalid(EFFECTS, format!("{} characters", value.len())));
    }
    let bytes = crate::monitor_image::base64url_decode(value)
        .ok_or_else(|| invalid(EFFECTS, format!("{} characters", value.len())))?;
    let mut reader = Reader {
        bytes: &bytes,
        position: 0,
    };
    let effects = reader
        .effects()
        .filter(|_| reader.position == reader.bytes.len())
        .ok_or_else(|| invalid(EFFECTS, format!("{} bytes", bytes.len())))?;
    validate_effect_list(&effects)?;
    Ok(effects)
}

fn push_layer(out: &mut Vec<u8>, layer: &WireSceneLayerOutline) {
    push_varint(out, u128::from(layer.id.get().get()));
    let mut flags = if layer.enabled { ENABLED } else { 0 };
    if layer.crop.is_some() {
        flags |= CROPPED;
    }
    if layer.key.is_some() {
        flags |= KEYED;
    }
    if let Some(mask) = layer.mask {
        flags |= MASKED;
        if mask.invert {
            flags |= MASK_INVERTED;
        }
    }
    out.push(flags);
    out.push(layer.quarter_turns);
    push_varint(out, layer.name.len() as u128);
    out.extend_from_slice(layer.name.as_bytes());
    let (kind, id) = match layer.source {
        WireSceneLayerSource::Input(input) => (SOURCE_INPUT, input.get()),
        WireSceneLayerSource::Scene(scene) => (SOURCE_SCENE, scene.get()),
    };
    out.push(kind);
    push_varint(out, id.get());
    push_varint(out, u128::from(zigzag(i64::from(layer.x))));
    push_varint(out, u128::from(zigzag(i64::from(layer.y))));
    push_varint(out, u128::from(layer.width));
    push_varint(out, u128::from(layer.height));
    out.push(layer.opacity);
    push_varint(out, u128::from(zigzag(i64::from(layer.tilt_millidegrees))));
    push_varint(out, u128::from(layer.anchor_x));
    push_varint(out, u128::from(layer.anchor_y));
    push_varint(out, u128::from(zigzag(i64::from(layer.z_order))));
    if let Some(crop) = layer.crop {
        for value in [crop.x, crop.y, crop.width, crop.height] {
            push_varint(out, u128::from(value));
        }
    }
    if let Some(mask) = layer.mask {
        for value in [mask.x, mask.y, mask.width, mask.height] {
            push_varint(out, u128::from(value));
        }
    }
    if let Some(key) = layer.key {
        push_key(out, key);
    }
    push_effects(out, &layer.effects);
}

fn layers_bytes(layout: &WireSceneLayout) -> Vec<u8> {
    let mut out = Vec::new();
    push_varint(&mut out, layout.layers.len() as u128);
    for layer in &layout.layers {
        push_layer(&mut out, layer);
    }
    out
}

/// The size of `layout`'s layer list encoding, in bytes; a layout whose
/// size exceeds [`MAX_SCENE_LAYOUT_BYTES`] cannot be carried.
#[must_use]
pub fn scene_layout_wire_bytes(layout: &WireSceneLayout) -> usize {
    layers_bytes(layout).len()
}

/// Enforces a spec: the layer's design bounds (keys and effects included),
/// a nonblank name, and nonzero sides.
fn validate_spec(layer: &WireSceneLayerOutline) -> Result<(), CodecError> {
    if layer.name.trim().is_empty()
        || layer.name.len() > MAX_SCENE_LAYER_NAME_BYTES
        || layer.name.chars().any(char::is_control)
    {
        return Err(invalid(SPEC, format!("layer {} name", layer.id)));
    }
    if layer.width == 0 || layer.height == 0 {
        return Err(invalid(SPEC, format!("layer {} is empty", layer.id)));
    }
    validate_design(SPEC, layer, None)
}

/// Validates and encodes a `scene_layer_add` spec.
///
/// # Errors
///
/// [`CodecError::InvalidField`] naming `spec` for a spec outside the bounds
/// of the module documentation.
pub(super) fn encode_layer_spec(spec: &WireSceneLayerOutline) -> Result<String, CodecError> {
    validate_spec(spec)?;
    let mut out = Vec::new();
    push_layer(&mut out, spec);
    Ok(crate::monitor_image::base64url_encode(&out))
}

/// Strictly decodes a `scene_layer_add` spec.
///
/// # Errors
///
/// [`CodecError::InvalidField`] naming `spec` for non-canonical base64url,
/// a truncated or trailing encoding, or any bound of the module
/// documentation.
pub(super) fn decode_layer_spec(value: &str) -> Result<WireSceneLayerOutline, CodecError> {
    let encoded_limit = (MAX_SCENE_LAYOUT_BYTES * 4).div_ceil(3);
    if value.len() > encoded_limit {
        return Err(invalid(SPEC, format!("{} characters", value.len())));
    }
    let bytes = crate::monitor_image::base64url_decode(value)
        .ok_or_else(|| invalid(SPEC, format!("{} characters", value.len())))?;
    let mut reader = Reader {
        bytes: &bytes,
        position: 0,
    };
    let spec = reader
        .scene_layer()
        .filter(|_| reader.position == reader.bytes.len())
        .ok_or_else(|| invalid(SPEC, format!("{} bytes", bytes.len())))?;
    validate_spec(&spec)?;
    Ok(spec)
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
    fn text(&mut self, limit: usize) -> Option<String> {
        let length = usize::try_from(self.varint(32)?).ok()?;
        if length > limit {
            return None;
        }
        let end = self.position.checked_add(length)?;
        let text = core::str::from_utf8(self.bytes.get(self.position..end)?)
            .ok()?
            .to_owned();
        self.position = end;
        Some(text)
    }

    fn rect(&mut self) -> Option<[u32; 4]> {
        Some([self.u32()?, self.u32()?, self.u32()?, self.u32()?])
    }

    fn effects(&mut self) -> Option<Vec<WireLayerEffect>> {
        let count = usize::try_from(self.varint(32)?).ok()?;
        if count > MAX_SCENE_LAYER_EFFECTS {
            return None;
        }
        let mut effects = Vec::with_capacity(count);
        for _ in 0..count {
            let (name, ranges) = SCENE_LAYER_EFFECT_KINDS.get(usize::from(self.byte()?))?;
            let parameters = ranges
                .iter()
                .map(|_| self.i32())
                .collect::<Option<Vec<_>>>()?;
            effects.push(WireLayerEffect {
                name: (*name).to_owned(),
                parameters,
            });
        }
        Some(effects)
    }

    fn flag(&mut self) -> Option<bool> {
        match self.byte()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    fn refinement(&mut self) -> Option<WireKeyRefinement> {
        let clip_black = self.byte()?;
        let clip_white = self.byte()?;
        let choke = i8::try_from(self.i32()?).ok()?;
        let edge_softness = self.byte()?;
        let despill = match (self.byte()?, self.byte()?) {
            (DESPILL_OFF, 0) => WireKeyDespill::Off,
            (DESPILL_GREEN, strength) => WireKeyDespill::Green { strength },
            (DESPILL_BLUE, strength) => WireKeyDespill::Blue { strength },
            _ => return None,
        };
        Some(WireKeyRefinement {
            clip_black,
            clip_white,
            choke,
            edge_softness,
            despill,
            light_wrap: self.byte()?,
            light_wrap_width: self.byte()?,
            garbage_matte: self.garbage_matte()?,
        })
    }

    /// A refinement's garbage matte (2.97): `Some(None)` for none.
    #[allow(clippy::option_option)] // malformed, none, or a matte
    fn garbage_matte(&mut self) -> Option<Option<fm_types::GarbageMatte>> {
        let header = self.byte()?;
        if header == 0 {
            return Some(None);
        }
        let count = usize::from(header >> 1);
        if !(fm_types::MIN_GARBAGE_MATTE_POINTS..=fm_types::MAX_GARBAGE_MATTE_POINTS)
            .contains(&count)
        {
            return None;
        }
        let mut coordinates = Vec::with_capacity(count * 2);
        let mut bits = 0_u32;
        let mut pending = 0_u32;
        while coordinates.len() < count * 2 {
            while pending < MATTE_COORDINATE_BITS {
                bits |= u32::from(self.byte()?) << pending;
                pending += 8;
            }
            coordinates.push(u16::try_from(bits & u32::from(MATTE_COORDINATE_MASK)).ok()?);
            bits >>= MATTE_COORDINATE_BITS;
            pending -= MATTE_COORDINATE_BITS;
        }
        // Spare bits of the last byte are zero, so each matte has exactly
        // one encoding.
        if bits != 0 {
            return None;
        }
        let points: Vec<_> = coordinates
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[x, y]| fm_types::MattePoint::new(x, y))
            .collect();
        fm_types::GarbageMatte::new(&points, header & 1 == 1)
            .ok()
            .map(Some)
    }

    fn key(&mut self) -> Option<WireLayerKey> {
        Some(match self.byte()? {
            KEY_CHROMA => WireLayerKey::Chroma(WireChromaKey {
                red: self.byte()?,
                green: self.byte()?,
                blue: self.byte()?,
                similarity: self.byte()?,
                smoothness: self.byte()?,
                spill: self.byte()?,
                refinement: self.refinement()?,
            }),
            KEY_LUMA => WireLayerKey::Luma(WireLumaKey {
                low: self.byte()?,
                high: self.byte()?,
                softness: self.byte()?,
                invert: self.flag()?,
                refinement: self.refinement()?,
            }),
            KEY_FILL => WireLayerKey::Fill(WireFillKey {
                key_input: WireInputId::new(core::num::NonZeroU128::new(self.varint(128)?)?),
                alpha_channel: self.flag()?,
                low: self.byte()?,
                high: self.byte()?,
                softness: self.byte()?,
                invert: self.flag()?,
                refinement: self.refinement()?,
            }),
            _ => return None,
        })
    }

    fn scene_layer(&mut self) -> Option<WireSceneLayerOutline> {
        let id = WireSceneLayerId::new(NonZeroU32::new(self.u32()?)?);
        let flags = self.byte()?;
        if flags & !(ENABLED | CROPPED | MASKED | MASK_INVERTED | KEYED) != 0
            || (flags & MASK_INVERTED != 0 && flags & MASKED == 0)
        {
            return None;
        }
        let enabled = flags & ENABLED != 0;
        let quarter_turns = self.byte()?;
        let name = self.text(MAX_SCENE_LAYER_NAME_BYTES)?;
        let kind = self.byte()?;
        let source = core::num::NonZeroU128::new(self.varint(128)?)?;
        let source = match kind {
            SOURCE_INPUT => WireSceneLayerSource::Input(WireInputId::new(source)),
            SOURCE_SCENE => WireSceneLayerSource::Scene(WireSceneId::new(source)),
            _ => return None,
        };
        let x = self.i32()?;
        let y = self.i32()?;
        let width = self.u32()?;
        let height = self.u32()?;
        let opacity = self.byte()?;
        let tilt_millidegrees = self.i32()?;
        let anchor_x = u16::try_from(self.varint(16)?).ok()?;
        let anchor_y = u16::try_from(self.varint(16)?).ok()?;
        let z_order = self.i32()?;
        let crop = if flags & CROPPED == 0 {
            None
        } else {
            let [x, y, width, height] = self.rect()?;
            Some(WireLayerCrop {
                x,
                y,
                width,
                height,
            })
        };
        let mask = if flags & MASKED == 0 {
            None
        } else {
            let [x, y, width, height] = self.rect()?;
            Some(WireLayerMask {
                x,
                y,
                width,
                height,
                invert: flags & MASK_INVERTED != 0,
            })
        };
        let key = if flags & KEYED == 0 {
            None
        } else {
            Some(self.key()?)
        };
        let effects = self.effects()?;
        Some(WireSceneLayerOutline {
            id,
            name,
            source,
            x,
            y,
            width,
            height,
            quarter_turns,
            tilt_millidegrees,
            anchor_x,
            anchor_y,
            opacity,
            enabled,
            z_order,
            crop,
            mask,
            key,
            effects,
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

const LAYOUT_SCENES: &str = "layout_scenes";

/// Encodes a snapshot's `layout_scenes` list (possibly empty): strictly
/// ascending scenes, at most [`super::MAX_LIST_ITEMS`].
pub(super) fn encode_layout_scenes(scenes: &[WireSceneId]) -> Result<String, CodecError> {
    if scenes.len() > super::MAX_LIST_ITEMS {
        return Err(CodecError::TooManyItems(LAYOUT_SCENES));
    }
    let joined = scenes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if !scenes.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(invalid(LAYOUT_SCENES, joined));
    }
    Ok(joined)
}

/// Strictly decodes a snapshot's `layout_scenes` list.
pub(super) fn decode_layout_scenes(value: &str) -> Result<Vec<WireSceneId>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.split(',').nth(super::MAX_LIST_ITEMS).is_some() {
        return Err(CodecError::TooManyItems(LAYOUT_SCENES));
    }
    let scenes = value
        .split(',')
        .map(|entry| parse_canonical_scene(entry).ok_or_else(|| invalid(LAYOUT_SCENES, entry)))
        .collect::<Result<Vec<_>, _>>()?;
    if !scenes.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(invalid(LAYOUT_SCENES, value));
    }
    Ok(scenes)
}

/// Parses a canonical nonzero scene identifier as written by its `Display`.
pub(super) fn parse_canonical_scene(value: &str) -> Option<WireSceneId> {
    super::value::parse_canonical_input(value).map(|input| WireSceneId::new(input.get()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn matte_refinement(matte: Option<fm_types::GarbageMatte>) -> WireKeyRefinement {
        WireKeyRefinement {
            garbage_matte: matte,
            ..WireKeyRefinement::NONE
        }
    }

    fn refinement_bytes(refinement: WireKeyRefinement) -> Vec<u8> {
        let mut out = Vec::new();
        push_key(
            &mut out,
            WireLayerKey::Luma(crate::WireLumaKey {
                low: 0,
                high: 255,
                softness: 0,
                invert: false,
                refinement,
            }),
        );
        out
    }

    #[allow(clippy::option_option)]
    fn read_matte(bytes: &[u8]) -> Option<Option<fm_types::GarbageMatte>> {
        let mut reader = Reader { bytes, position: 0 };
        let matte = reader.garbage_matte()?;
        (reader.position == bytes.len()).then_some(matte)
    }

    #[test]
    fn garbage_matte_points_pack_into_ten_bit_coordinates() {
        let matte = fm_types::GarbageMatte::new(
            &[
                fm_types::MattePoint::new(1, 2),
                fm_types::MattePoint::new(1_000, 0),
                fm_types::MattePoint::new(0, 1_000),
            ],
            true,
        )
        .unwrap();
        let none = refinement_bytes(matte_refinement(None));
        let bytes = refinement_bytes(matte_refinement(Some(matte)));
        let packed = &bytes[none.len() - 1..];
        // Header 3 << 1 | outside, then 60 bits of 1, 2, 1000, 0, 0, 1000
        // least significant bit first, in eight bytes.
        let bits: u64 = 1 | 2 << 10 | 1_000 << 20 | 1_000 << 50;
        let mut expected = vec![7];
        expected.extend_from_slice(&bits.to_le_bytes());
        assert_eq!(packed, expected);
        assert_eq!(read_matte(packed), Some(Some(matte)));
        assert_eq!(read_matte(&[0]), Some(None));

        // Spare bits, a short stream, and bad counts are refused.
        let mut spare = expected.clone();
        spare[8] |= 0x10;
        assert_eq!(read_matte(&spare), None);
        assert_eq!(read_matte(&expected[..8]), None);
        let mut few = expected.clone();
        few[0] = 2 << 1;
        assert_eq!(read_matte(&few[..6]), None);
        assert_eq!(read_matte(&[9 << 1]), None);
        // A coordinate beyond 1000 is refused.
        let mut far = expected;
        far[1] = 0xe9;
        far[2] |= 0x03;
        assert_eq!(read_matte(&far), None);
    }
}
