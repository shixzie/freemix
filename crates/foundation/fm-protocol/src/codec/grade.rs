//! Grading values (Protocol 2.28): grades, grade sources, LUT metadata, and
//! LUT text.
//!
//! * A grade id (`grade_id`, `library`) and a LUT id (`lut_id`) are canonical
//!   nonzero 32-bit decimals.
//! * `name` is nonblank, at most [`MAX_GRADE_NAME_BYTES`] bytes, and free of
//!   control characters.
//! * `kind` is `1d` or `3d`; `size` is the LUT's entry count (1D) or edge
//!   length (3D) within the `.cube` bounds.
//! * `data` is the `.cube` text, base64url without padding, at most
//!   [`MAX_GRADE_LUT_IMPORT_BYTES`] bytes before encoding.
//! * `grade` is seventeen `;`-separated sections:
//!
//!   ```text
//!   enabled;temperature,tint;exposure;contrast,pivot;saturation;
//!   lift;gamma;gain;offset;master;red;green;blue;
//!   hue_vs_hue;hue_vs_saturation;luma_vs_saturation;lut
//!   ```
//!
//!   `enabled` is `0` or `1`; each wheel is `x,y,master`; each RGB curve is
//!   `x:y` points joined by `,`; each hue curve is empty or `position:value`
//!   points joined by `,`; `lut` is `-` or `<lut id>:<strength permille>`,
//!   naming the imported LUT `asset://luts/<lut id>.cube`. Numbers are
//!   canonical decimals (a `-` sign only on nonzero values). Every
//!   [`fm_grade::Grade`] bound is enforced on encode and decode.

use core::num::NonZeroU32;

use fm_grade::{
    Contrast, CubeKind, Curve, CurvePoint, Grade, GradeLut, HueCurve, HueCurves, HuePoint,
    LUT_URI_PREFIX, MAX_CUBE_1D_SIZE, MAX_CUBE_3D_EDGE, MIN_CUBE_SIZE, RgbCurves, Wheel, Wheels,
    WhiteBalance,
};

use crate::{CodecError, MAX_GRADE_LUT_IMPORT_BYTES, MAX_GRADE_NAME_BYTES};

use super::value::parse_canonical_u64;

const GRADE: &str = "grade";

fn invalid(field: &'static str, value: &str) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.to_owned(),
    }
}

/// Parses a canonical nonzero 32-bit identifier.
pub(super) fn parse_grade_id(field: &'static str, value: &str) -> Result<NonZeroU32, CodecError> {
    parse_canonical_u64(value)
        .and_then(|value| u32::try_from(value).ok())
        .and_then(NonZeroU32::new)
        .ok_or_else(|| invalid(field, value))
}

/// Enforces the grade and LUT name rule shared by the encoder and decoder.
pub(super) fn validate_grade_name(name: &str) -> Result<(), CodecError> {
    if name.trim().is_empty()
        || name.len() > MAX_GRADE_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        Err(invalid("name", name))
    } else {
        Ok(())
    }
}

/// Enforces the animation preset name rule (Protocol 2.34): the grade name
/// rule with [`crate::MAX_ANIMATION_PRESET_NAME_BYTES`].
pub(super) fn validate_preset_name(name: &str) -> Result<(), CodecError> {
    if name.trim().is_empty()
        || name.len() > crate::MAX_ANIMATION_PRESET_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        Err(invalid("name", name))
    } else {
        Ok(())
    }
}

pub(super) const fn encode_cube_kind(kind: CubeKind) -> &'static str {
    match kind {
        CubeKind::OneD => "1d",
        CubeKind::ThreeD => "3d",
    }
}

pub(super) fn decode_cube_kind(value: &str) -> Result<CubeKind, CodecError> {
    match value {
        "1d" => Ok(CubeKind::OneD),
        "3d" => Ok(CubeKind::ThreeD),
        _ => Err(invalid("kind", value)),
    }
}

/// Enforces the `.cube` size bound for `kind`.
pub(super) fn validate_lut_size(kind: CubeKind, size: u32) -> Result<(), CodecError> {
    let maximum = match kind {
        CubeKind::OneD => MAX_CUBE_1D_SIZE,
        CubeKind::ThreeD => MAX_CUBE_3D_EDGE,
    };
    if usize::try_from(size).is_ok_and(|size| (MIN_CUBE_SIZE..=maximum).contains(&size)) {
        Ok(())
    } else {
        Err(invalid("size", &size.to_string()))
    }
}

const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Encodes nonempty `.cube` text of at most [`MAX_GRADE_LUT_IMPORT_BYTES`]
/// as unpadded base64url.
pub(super) fn encode_lut_data(text: &str) -> Result<String, CodecError> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_GRADE_LUT_IMPORT_BYTES {
        return Err(invalid("data", &format!("{} bytes", bytes.len())));
    }
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |group, (index, byte)| {
                group | (u32::from(*byte) << (16 - 8 * index))
            });
        for index in 0..=chunk.len() {
            let sextet = (group >> (18 - 6 * index)) & 0x3f;
            encoded.push(char::from(BASE64URL[sextet as usize]));
        }
    }
    Ok(encoded)
}

/// Strictly decodes unpadded base64url `.cube` text: canonical (unused
/// trailing bits are zero), nonempty, at most
/// [`MAX_GRADE_LUT_IMPORT_BYTES`] bytes, and UTF-8.
pub(super) fn decode_lut_data(value: &str) -> Result<String, CodecError> {
    let too_long = || invalid("data", &format!("{} encoded bytes", value.len()));
    if value.is_empty() || value.len() % 4 == 1 {
        return Err(invalid("data", value));
    }
    if value.len() > MAX_GRADE_LUT_IMPORT_BYTES.div_ceil(3) * 4 {
        return Err(too_long());
    }
    let sextet = |byte: u8| -> Option<u32> {
        BASE64URL
            .iter()
            .position(|candidate| *candidate == byte)
            .and_then(|position| u32::try_from(position).ok())
    };
    let mut decoded = Vec::with_capacity(value.len() / 4 * 3 + 2);
    for chunk in value.as_bytes().chunks(4) {
        let mut group = 0_u32;
        for (index, byte) in chunk.iter().enumerate() {
            let bits = sextet(*byte).ok_or_else(|| invalid("data", value))?;
            group |= bits << (18 - 6 * index);
        }
        let bytes = chunk.len() - 1;
        let unused = 24 - 8 * u32::try_from(bytes).unwrap_or(3);
        if bytes < 3 && group & ((1 << unused) - 1) != 0 {
            return Err(invalid("data", value));
        }
        for index in 0..bytes {
            let shift = 16 - 8 * u32::try_from(index).unwrap_or(0);
            decoded.push(u8::try_from((group >> shift) & 0xff).unwrap_or(0));
        }
    }
    if decoded.len() > MAX_GRADE_LUT_IMPORT_BYTES {
        return Err(too_long());
    }
    String::from_utf8(decoded).map_err(|_| CodecError::InvalidUtf8)
}

/// The LUT id a grade's LUT reference names when it is exactly
/// `asset://luts/<id>.cube`.
fn lut_reference_id(uri: &str) -> Option<NonZeroU32> {
    let digits = uri.strip_prefix(LUT_URI_PREFIX)?.strip_suffix(".cube")?;
    parse_grade_id("grade", digits).ok()
}

/// Encodes a valid grade whose LUT, if any, is a canonical imported-LUT
/// reference.
pub(super) fn encode_grade(grade: &Grade) -> Result<String, CodecError> {
    grade
        .validate()
        .map_err(|error| invalid(GRADE, &error.to_string()))?;
    let wheel = |wheel: Wheel| format!("{},{},{}", wheel.x, wheel.y, wheel.master);
    let curve = |curve: &Curve| {
        curve
            .points()
            .iter()
            .map(|point| format!("{}:{}", point.x, point.y))
            .collect::<Vec<_>>()
            .join(",")
    };
    let hue = |curve: &HueCurve| {
        curve
            .points()
            .iter()
            .map(|point| format!("{}:{}", point.position, point.value))
            .collect::<Vec<_>>()
            .join(",")
    };
    let lut = match &grade.lut {
        None => "-".to_owned(),
        Some(lut) => {
            let id =
                lut_reference_id(&lut.asset_uri).ok_or_else(|| invalid(GRADE, &lut.asset_uri))?;
            format!("{id}:{}", lut.strength_permille)
        }
    };
    Ok([
        u8::from(grade.enabled).to_string(),
        format!(
            "{},{}",
            grade.white_balance.temperature, grade.white_balance.tint
        ),
        grade.exposure_millistops.to_string(),
        format!("{},{}", grade.contrast.amount, grade.contrast.pivot_bp),
        grade.saturation_permille.to_string(),
        wheel(grade.wheels.lift),
        wheel(grade.wheels.gamma),
        wheel(grade.wheels.gain),
        wheel(grade.wheels.offset),
        curve(&grade.curves.master),
        curve(&grade.curves.red),
        curve(&grade.curves.green),
        curve(&grade.curves.blue),
        hue(&grade.hue_curves.hue_vs_hue),
        hue(&grade.hue_curves.hue_vs_saturation),
        hue(&grade.hue_curves.luma_vs_saturation),
        lut,
    ]
    .join(";"))
}

/// Parses a canonical signed 16-bit decimal: an optional `-` (never on
/// zero), then digits without a leading zero.
fn parse_i16(value: &str) -> Option<i16> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    let negative = digits.len() != value.len();
    let magnitude = parse_canonical_u64(digits)?;
    if negative && magnitude == 0 {
        return None;
    }
    let magnitude = i64::try_from(magnitude).ok()?;
    i16::try_from(if negative { -magnitude } else { magnitude }).ok()
}

fn parse_u16(value: &str) -> Option<u16> {
    u16::try_from(parse_canonical_u64(value)?).ok()
}

fn parse_pair<A, B>(
    value: &str,
    separator: char,
    first: impl Fn(&str) -> Option<A>,
    second: impl Fn(&str) -> Option<B>,
) -> Option<(A, B)> {
    let (left, right) = value.split_once(separator)?;
    Some((first(left)?, second(right)?))
}

fn parse_wheel(value: &str) -> Option<Wheel> {
    let parts = value.split(',').collect::<Vec<_>>();
    let [x, y, master] = parts[..] else {
        return None;
    };
    Some(Wheel::new(parse_i16(x)?, parse_i16(y)?, parse_i16(master)?))
}

fn parse_curve(value: &str) -> Option<Curve> {
    if value.is_empty() || value.split(',').count() > fm_grade::MAX_CURVE_POINTS {
        return None;
    }
    value
        .split(',')
        .map(|point| {
            parse_pair(point, ':', parse_u16, parse_u16).map(|(x, y)| CurvePoint::new(x, y))
        })
        .collect::<Option<Vec<_>>>()
        .map(Curve::new)
}

fn parse_hue_curve(value: &str) -> Option<HueCurve> {
    if value.is_empty() {
        return Some(HueCurve::EMPTY);
    }
    if value.split(',').count() > fm_grade::MAX_HUE_CURVE_POINTS {
        return None;
    }
    value
        .split(',')
        .map(|point| {
            parse_pair(point, ':', parse_u16, parse_i16)
                .map(|(position, value)| HuePoint::new(position, value))
        })
        .collect::<Option<Vec<_>>>()
        .map(HueCurve::new)
}

fn parse_grade(value: &str) -> Option<Grade> {
    let sections = value.split(';').collect::<Vec<_>>();
    let [
        enabled,
        white_balance,
        exposure,
        contrast,
        saturation,
        lift,
        gamma,
        gain,
        offset,
        master,
        red,
        green,
        blue,
        hue_vs_hue,
        hue_vs_saturation,
        luma_vs_saturation,
        lut,
    ] = sections[..]
    else {
        return None;
    };
    let (temperature, tint) = parse_pair(white_balance, ',', parse_i16, parse_i16)?;
    let (amount, pivot_bp) = parse_pair(contrast, ',', parse_i16, parse_u16)?;
    let lut = match lut {
        "-" => None,
        lut => {
            let (id, strength) =
                parse_pair(lut, ':', |id| parse_grade_id(GRADE, id).ok(), parse_u16)?;
            Some(GradeLut::new(
                format!("{LUT_URI_PREFIX}{id}.cube"),
                strength,
            ))
        }
    };
    Some(Grade {
        enabled: match enabled {
            "0" => false,
            "1" => true,
            _ => return None,
        },
        white_balance: WhiteBalance::new(temperature, tint),
        exposure_millistops: parse_i16(exposure)?,
        contrast: Contrast::new(amount, pivot_bp),
        saturation_permille: parse_u16(saturation)?,
        wheels: Wheels {
            lift: parse_wheel(lift)?,
            gamma: parse_wheel(gamma)?,
            gain: parse_wheel(gain)?,
            offset: parse_wheel(offset)?,
        },
        curves: RgbCurves {
            master: parse_curve(master)?,
            red: parse_curve(red)?,
            green: parse_curve(green)?,
            blue: parse_curve(blue)?,
        },
        hue_curves: HueCurves {
            hue_vs_hue: parse_hue_curve(hue_vs_hue)?,
            hue_vs_saturation: parse_hue_curve(hue_vs_saturation)?,
            luma_vs_saturation: parse_hue_curve(luma_vs_saturation)?,
        },
        lut,
    })
}

/// Strictly decodes a grade and enforces every [`fm_grade::Grade`] bound.
pub(super) fn decode_grade(value: &str) -> Result<Box<Grade>, CodecError> {
    let grade = parse_grade(value).ok_or_else(|| invalid(GRADE, value))?;
    grade
        .validate()
        .map_err(|error| invalid(GRADE, &error.to_string()))?;
    Ok(Box::new(grade))
}
