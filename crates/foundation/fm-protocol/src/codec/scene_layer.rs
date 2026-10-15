//! Live scene layer edit codec (Protocol 2.39, SW-006).
//!
//! Every `scene_layer_*` payload carries `scene` (canonical scene id),
//! `layer` (canonical nonzero layer id), and `continues` (whether it folds
//! into the gesture's undo step), then its operation's fields (see
//! [`crate::SceneLayerOperation`]): canonical decimals (signed values with a
//! leading `-` only when negative), booleans `true`/`false`, and the
//! `scene_layer_add` `spec` in the binary layer encoding of the scene
//! layout codec.

use core::num::NonZeroU32;

use crate::{
    CodecError, SceneLayerEdit, SceneLayerOperation, WireLayerCrop, WireLayerGeometry,
    WireLayerMask, WireSceneLayerId,
};

use super::media::{FieldSource, Push};
use super::scene_layout::{decode_layer_spec, encode_layer_spec, parse_canonical_scene};
use super::value::parse_canonical_u64;

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
        SceneLayerOperation::SetCrop(None)
        | SceneLayerOperation::SetMask(None)
        | SceneLayerOperation::Remove => Ok(()),
        SceneLayerOperation::SetZOrder { z_order } => push("z_order", z_order.to_string()),
        SceneLayerOperation::Move { index } => push("index", index.to_string()),
        SceneLayerOperation::Add { index, spec } => {
            if spec.layer.id != edit.layer {
                return Err(invalid("spec", format!("layer {}", spec.layer.id)));
            }
            push("index", index.to_string())?;
            push("spec", encode_layer_spec(spec)?)
        }
    }
}

/// Decodes the edit named by `payload`, or `None` for another payload.
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
        "scene_layer_z_order_set" => SceneLayerOperation::SetZOrder {
            z_order: i32_field(fields, "z_order")?,
        },
        "scene_layer_move" => SceneLayerOperation::Move {
            index: index(fields)?,
        },
        "scene_layer_add" => {
            let index = index(fields)?;
            let spec = decode_layer_spec(&fields.required("spec")?)?;
            if spec.layer.id != layer {
                return Err(invalid("spec", format!("layer {}", spec.layer.id)));
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
    }))
}
