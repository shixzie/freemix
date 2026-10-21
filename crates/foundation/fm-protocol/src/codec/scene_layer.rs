//! Live scene layer edit codec (Protocol 2.39, SW-006; `gesture` since
//! 2.41; keys and effects since 2.43).
//!
//! Every `scene_layer_*` payload carries `scene` (canonical scene id),
//! `layer` (canonical nonzero layer id), `continues` (whether it folds into
//! the gesture's undo step), and `gesture` (the canonical `u64` gesture
//! token, `0` for none), then its operation's fields (see
//! [`crate::SceneLayerOperation`]): canonical decimals (signed values with a
//! leading `-` only when negative), booleans `true`/`false`, and the
//! `scene_layer_add` `spec` and the `scene_layer_effects_set` `effects` in
//! the binary encodings of the scene layout codec. A `scene_layer_key_set`
//! names its `kind` (`chroma`, `luma`, or `fill` since 2.70) and that kind's
//! fields, then (since 2.70) the refinement's `clip_black`, `clip_white`,
//! `choke` (signed), `edge_softness`, `despill` (`off`, `green`, or `blue`),
//! and `despill_strength` (`0` when `despill` is `off`), then (since 2.97)
//! `light_wrap` and `light_wrap_width` (`0` exactly when `light_wrap` is),
//! and `matte` (`off`, `inside`, or `outside`) with, unless `off`,
//! `matte_points`: three to eight canonical `x:y` points (each
//! `0..=1000`) joined by commas.

use core::num::NonZeroU32;

use crate::{
    CodecError, SceneLayerEdit, SceneLayerOperation, WireChromaKey, WireFillKey, WireKeyDespill,
    WireKeyRefinement, WireLayerCrop, WireLayerGeometry, WireLayerKey, WireLayerMask, WireLumaKey,
    WireSceneLayerId,
};

use super::media::{FieldSource, Push};
use super::scene_layout::{
    decode_effect_list, decode_layer_spec, encode_effect_list, encode_layer_spec,
    parse_canonical_scene,
};
use super::value::{parse_canonical_input, parse_canonical_u64};

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn unsigned(fields: &mut dyn FieldSource, name: &'static str, max: u64) -> Result<u64, CodecError> {
    let value = fields.required(name)?;
    parse_canonical_u64(&value)
        .filter(|number| *number <= max)
        .ok_or_else(|| invalid(name, value))
}

/// An unsigned field of at most `max`, as its own type.
fn bounded<T>(fields: &mut dyn FieldSource, name: &'static str, max: T) -> Result<T, CodecError>
where
    T: Copy + TryFrom<u64>,
    u64: From<T>,
{
    let value = unsigned(fields, name, u64::from(max))?;
    T::try_from(value).map_err(|_| invalid(name, value.to_string()))
}

fn u32_field(fields: &mut dyn FieldSource, name: &'static str) -> Result<u32, CodecError> {
    bounded(fields, name, u32::MAX)
}

fn i32_field(fields: &mut dyn FieldSource, name: &'static str) -> Result<i32, CodecError> {
    let value = fields.required(name)?;
    let (negative, digits) = value
        .strip_prefix('-')
        .map_or((false, value.as_str()), |digits| (true, digits));
    parse_canonical_u64(digits)
        .filter(|magnitude| !(negative && *magnitude == 0))
        .and_then(|magnitude| i64::try_from(magnitude).ok())
        .map(|magnitude| if negative { -magnitude } else { magnitude })
        .and_then(|number| i32::try_from(number).ok())
        .ok_or_else(|| invalid(name, value))
}

fn bool_field(fields: &mut dyn FieldSource, name: &'static str) -> Result<bool, CodecError> {
    let value = fields.required(name)?;
    match value.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid(name, value)),
    }
}

fn rect_fields(fields: &mut dyn FieldSource) -> Result<[u32; 4], CodecError> {
    let rect = [
        u32_field(fields, "x")?,
        u32_field(fields, "y")?,
        u32_field(fields, "width")?,
        u32_field(fields, "height")?,
    ];
    if rect[2] == 0 {
        return Err(invalid("width", "0"));
    }
    if rect[3] == 0 {
        return Err(invalid("height", "0"));
    }
    Ok(rect)
}

fn push_rect(push: &mut Push<'_>, [x, y, width, height]: [u32; 4]) -> Result<(), CodecError> {
    if width == 0 {
        return Err(invalid("width", "0"));
    }
    if height == 0 {
        return Err(invalid("height", "0"));
    }
    push("x", x.to_string())?;
    push("y", y.to_string())?;
    push("width", width.to_string())?;
    push("height", height.to_string())
}

/// Encodes an edit's `scene`, `layer`, and operation fields (not the
/// payload name).
pub(super) fn encode_scene_layer_edit(
    edit: &SceneLayerEdit,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    push("scene", edit.scene.to_string())?;
    push("layer", edit.layer.to_string())?;
    push("continues", edit.continues.to_string())?;
    push("gesture", edit.gesture.to_string())?;
    match &edit.operation {
        SceneLayerOperation::SetGeometry(geometry) => {
            if !geometry.is_valid() {
                return Err(invalid("geometry", format!("{geometry:?}")));
            }
            push("x", geometry.x.to_string())?;
            push("y", geometry.y.to_string())?;
            push("width", geometry.width.to_string())?;
            push("height", geometry.height.to_string())?;
            push("quarter_turns", geometry.quarter_turns.to_string())?;
            push("tilt_millidegrees", geometry.tilt_millidegrees.to_string())?;
            push("anchor_x", geometry.anchor_x.to_string())?;
            push("anchor_y", geometry.anchor_y.to_string())
        }
        SceneLayerOperation::SetAppearance { enabled, opacity } => {
            push("enabled", enabled.to_string())?;
            push("opacity", opacity.to_string())
        }
        SceneLayerOperation::SetCrop(Some(crop)) => {
            push_rect(push, [crop.x, crop.y, crop.width, crop.height])
        }
        SceneLayerOperation::SetMask(Some(mask)) => {
            push_rect(push, [mask.x, mask.y, mask.width, mask.height])?;
            push("invert", mask.invert.to_string())
        }
        SceneLayerOperation::SetKey(Some(key)) => {
            if !key.is_valid() {
                return Err(invalid("key", format!("{key:?}")));
            }
            match key {
                WireLayerKey::Chroma(chroma) => {
                    push("kind", "chroma".to_owned())?;
                    push("red", chroma.red.to_string())?;
                    push("green", chroma.green.to_string())?;
                    push("blue", chroma.blue.to_string())?;
                    push("similarity", chroma.similarity.to_string())?;
                    push("smoothness", chroma.smoothness.to_string())?;
                    push("spill", chroma.spill.to_string())?;
                }
                WireLayerKey::Luma(luma) => {
                    push("kind", "luma".to_owned())?;
                    push("low", luma.low.to_string())?;
                    push("high", luma.high.to_string())?;
                    push("softness", luma.softness.to_string())?;
                    push("invert", luma.invert.to_string())?;
                }
                WireLayerKey::Fill(fill) => {
                    push("kind", "fill".to_owned())?;
                    push("key_input", fill.key_input.to_string())?;
                    push(
                        "channel",
                        if fill.alpha_channel { "alpha" } else { "luma" }.to_owned(),
                    )?;
                    push("low", fill.low.to_string())?;
                    push("high", fill.high.to_string())?;
                    push("softness", fill.softness.to_string())?;
                    push("invert", fill.invert.to_string())?;
                }
            }
            push_refinement(push, key.refinement())
        }
        SceneLayerOperation::SetEffects(effects) => push("effects", encode_effect_list(effects)?),
        SceneLayerOperation::SetCrop(None)
        | SceneLayerOperation::SetMask(None)
        | SceneLayerOperation::SetKey(None)
        | SceneLayerOperation::Remove => Ok(()),
        SceneLayerOperation::SetZOrder { z_order } => push("z_order", z_order.to_string()),
        SceneLayerOperation::Move { index } => push("index", index.to_string()),
        SceneLayerOperation::Add { index, spec } => {
            if spec.id != edit.layer {
                return Err(invalid("spec", format!("layer {}", spec.id)));
            }
            push("index", index.to_string())?;
            push("spec", encode_layer_spec(spec)?)
        }
    }
}

/// Decodes the edit named by `payload`, or `None` for another payload.
/// Pushes a key refinement's fields (Protocol 2.70).
fn push_refinement(push: &mut Push<'_>, refinement: WireKeyRefinement) -> Result<(), CodecError> {
    push("clip_black", refinement.clip_black.to_string())?;
    push("clip_white", refinement.clip_white.to_string())?;
    push("choke", refinement.choke.to_string())?;
    push("edge_softness", refinement.edge_softness.to_string())?;
    let (despill, strength) = match refinement.despill {
        WireKeyDespill::Off => ("off", 0),
        WireKeyDespill::Green { strength } => ("green", strength),
        WireKeyDespill::Blue { strength } => ("blue", strength),
    };
    push("despill", despill.to_owned())?;
    push("despill_strength", strength.to_string())?;
    push("light_wrap", refinement.light_wrap.to_string())?;
    push("light_wrap_width", refinement.light_wrap_width.to_string())?;
    match refinement.garbage_matte {
        None => push("matte", "off".to_owned()),
        Some(matte) => {
            push(
                "matte",
                if matte.invert() { "outside" } else { "inside" }.to_owned(),
            )?;
            let points = matte
                .points()
                .iter()
                .map(|point| format!("{}:{}", point.x, point.y))
                .collect::<Vec<_>>()
                .join(",");
            push("matte_points", points)
        }
    }
}

/// A `matte_points` value: canonical `x:y` pairs joined by commas.
fn matte_points(value: &str, invert: bool) -> Result<fm_types::GarbageMatte, CodecError> {
    let bad = || invalid("matte_points", value);
    let mut points = Vec::new();
    for pair in value.split(',') {
        let (x, y) = pair.split_once(':').ok_or_else(bad)?;
        let coordinate = |text: &str| {
            let parsed = text.parse::<u16>().map_err(|_| bad())?;
            if parsed.to_string() != text {
                return Err(bad());
            }
            Ok(parsed)
        };
        points.push(fm_types::MattePoint::new(coordinate(x)?, coordinate(y)?));
    }
    fm_types::GarbageMatte::new(&points, invert).map_err(|_| bad())
}

/// A key refinement's fields (Protocol 2.70).
fn refinement_fields(fields: &mut dyn FieldSource) -> Result<WireKeyRefinement, CodecError> {
    let clip_black = bounded(fields, "clip_black", u8::MAX)?;
    let clip_white = bounded(fields, "clip_white", u8::MAX)?;
    let choke_value = i32_field(fields, "choke")?;
    let choke = i8::try_from(choke_value).map_err(|_| invalid("choke", choke_value.to_string()))?;
    let edge_softness = bounded(fields, "edge_softness", u8::MAX)?;
    let despill = fields.required("despill")?;
    let strength = bounded(fields, "despill_strength", u8::MAX)?;
    let despill = match despill.as_str() {
        "off" if strength == 0 => WireKeyDespill::Off,
        "off" => return Err(invalid("despill_strength", strength.to_string())),
        "green" => WireKeyDespill::Green { strength },
        "blue" => WireKeyDespill::Blue { strength },
        _ => return Err(invalid("despill", despill)),
    };
    let light_wrap = bounded(fields, "light_wrap", u8::MAX)?;
    let light_wrap_width = bounded(fields, "light_wrap_width", u8::MAX)?;
    if (light_wrap == 0) != (light_wrap_width == 0)
        || light_wrap_width > crate::MAX_KEY_LIGHT_WRAP_WIDTH
    {
        return Err(invalid("light_wrap_width", light_wrap_width.to_string()));
    }
    let matte = fields.required("matte")?;
    let garbage_matte = match matte.as_str() {
        "off" => None,
        "inside" | "outside" => Some(matte_points(
            &fields.required("matte_points")?,
            matte == "outside",
        )?),
        _ => return Err(invalid("matte", matte)),
    };
    Ok(WireKeyRefinement {
        clip_black,
        clip_white,
        choke,
        edge_softness,
        despill,
        light_wrap,
        light_wrap_width,
        garbage_matte,
    })
}

/// The `kind` and that kind's fields of a `scene_layer_key_set`.
fn key_fields(fields: &mut dyn FieldSource) -> Result<WireLayerKey, CodecError> {
    let kind = fields.required("kind")?;
    let byte = |fields: &mut dyn FieldSource, name| bounded(fields, name, u8::MAX);
    let key = match kind.as_str() {
        "chroma" => WireLayerKey::Chroma(WireChromaKey {
            red: byte(fields, "red")?,
            green: byte(fields, "green")?,
            blue: byte(fields, "blue")?,
            similarity: byte(fields, "similarity")?,
            smoothness: byte(fields, "smoothness")?,
            spill: byte(fields, "spill")?,
            refinement: refinement_fields(fields)?,
        }),
        "luma" => WireLayerKey::Luma(WireLumaKey {
            low: byte(fields, "low")?,
            high: byte(fields, "high")?,
            softness: byte(fields, "softness")?,
            invert: bool_field(fields, "invert")?,
            refinement: refinement_fields(fields)?,
        }),
        "fill" => {
            let input = fields.required("key_input")?;
            let key_input =
                parse_canonical_input(&input).ok_or_else(|| invalid("key_input", input))?;
            let channel = fields.required("channel")?;
            let alpha_channel = match channel.as_str() {
                "luma" => false,
                "alpha" => true,
                _ => return Err(invalid("channel", channel)),
            };
            WireLayerKey::Fill(WireFillKey {
                key_input,
                alpha_channel,
                low: byte(fields, "low")?,
                high: byte(fields, "high")?,
                softness: byte(fields, "softness")?,
                invert: bool_field(fields, "invert")?,
                refinement: refinement_fields(fields)?,
            })
        }
        _ => return Err(invalid("kind", kind)),
    };
    if !key.is_valid() {
        return Err(invalid("key", format!("{key:?}")));
    }
    Ok(key)
}

pub(super) fn decode_scene_layer_edit(
    payload: &str,
    fields: &mut dyn FieldSource,
) -> Result<Option<SceneLayerEdit>, CodecError> {
    if !SceneLayerOperation::PAYLOADS.contains(&payload) {
        return Ok(None);
    }
    let scene_value = fields.required("scene")?;
    let scene = parse_canonical_scene(&scene_value).ok_or_else(|| invalid("scene", scene_value))?;
    let layer_value = u32_field(fields, "layer")?;
    let layer =
        WireSceneLayerId::new(NonZeroU32::new(layer_value).ok_or_else(|| invalid("layer", "0"))?);
    let continues = bool_field(fields, "continues")?;
    let gesture = unsigned(fields, "gesture", u64::MAX)?;
    let index = |fields: &mut dyn FieldSource| bounded(fields, "index", u16::MAX);
    let operation = match payload {
        "scene_layer_geometry_set" => {
            let geometry = WireLayerGeometry {
                x: i32_field(fields, "x")?,
                y: i32_field(fields, "y")?,
                width: u32_field(fields, "width")?,
                height: u32_field(fields, "height")?,
                quarter_turns: bounded(fields, "quarter_turns", 3_u8)?,
                tilt_millidegrees: i32_field(fields, "tilt_millidegrees")?,
                anchor_x: bounded(fields, "anchor_x", crate::SCENE_LAYER_ANCHOR_MAX)?,
                anchor_y: bounded(fields, "anchor_y", crate::SCENE_LAYER_ANCHOR_MAX)?,
            };
            if !geometry.is_valid() {
                return Err(invalid("geometry", format!("{geometry:?}")));
            }
            SceneLayerOperation::SetGeometry(geometry)
        }
        "scene_layer_appearance_set" => SceneLayerOperation::SetAppearance {
            enabled: bool_field(fields, "enabled")?,
            opacity: bounded(fields, "opacity", u8::MAX)?,
        },
        "scene_layer_crop_set" => {
            let [x, y, width, height] = rect_fields(fields)?;
            SceneLayerOperation::SetCrop(Some(WireLayerCrop {
                x,
                y,
                width,
                height,
            }))
        }
        "scene_layer_crop_clear" => SceneLayerOperation::SetCrop(None),
        "scene_layer_mask_set" => {
            let [x, y, width, height] = rect_fields(fields)?;
            SceneLayerOperation::SetMask(Some(WireLayerMask {
                x,
                y,
                width,
                height,
                invert: bool_field(fields, "invert")?,
            }))
        }
        "scene_layer_mask_clear" => SceneLayerOperation::SetMask(None),
        "scene_layer_key_set" => SceneLayerOperation::SetKey(Some(key_fields(fields)?)),
        "scene_layer_key_clear" => SceneLayerOperation::SetKey(None),
        "scene_layer_effects_set" => {
            SceneLayerOperation::SetEffects(decode_effect_list(&fields.required("effects")?)?)
        }
        "scene_layer_z_order_set" => SceneLayerOperation::SetZOrder {
            z_order: i32_field(fields, "z_order")?,
        },
        "scene_layer_move" => SceneLayerOperation::Move {
            index: index(fields)?,
        },
        "scene_layer_add" => {
            let index = index(fields)?;
            let spec = decode_layer_spec(&fields.required("spec")?)?;
            if spec.id != layer {
                return Err(invalid("spec", format!("layer {}", spec.id)));
            }
            SceneLayerOperation::Add {
                index,
                spec: Box::new(spec),
            }
        }
        "scene_layer_remove" => SceneLayerOperation::Remove,
        _ => unreachable!("every listed payload is handled"),
    };
    Ok(Some(SceneLayerEdit {
        scene,
        layer,
        operation,
        continues,
        gesture,
    }))
}
