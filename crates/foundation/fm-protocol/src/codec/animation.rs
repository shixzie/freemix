//! Title animation clips, phases, presets, and preview samples (Protocol
//! 2.29).
//!
//! A clip travels as one unpadded base64url field holding a compact binary
//! encoding, so no byte of it is percent-escaped by the record layer and the
//! bound is exact: at most [`MAX_TITLE_ANIMATION_BYTES`] bytes, which encode
//! to at most [`super::MAX_FIELD_VALUE_BYTES`] characters. Integers are
//! minimal unsigned LEB128 varints; signed values are zigzag-mapped first.
//!
//! ```text
//! clip   = rate_num rate_den in_frames out_frames track_count track*
//! track  = element(varint, nonzero u128) phase(byte: 0 in, 1 out)
//!          property(byte: index in fm_animation::Property::ALL) key_count key*
//! key    = frame value easing
//! value  = 4 bytes r g b a (color) | zigzag varint (every other property)
//! easing = tag byte in fm_animation::Easing declaration order
//!          (0 linear, 1 step, 2..=7 quad/cubic, 8 bezier, 9 hold,
//!          10..=18 sine/back/bounce); bezier is followed by
//!          x1 varint, y1 zigzag, x2 varint, y2 zigzag (Q16).
//! ```
//!
//! Tracks appear in strictly ascending (element, phase, property) order and
//! keys in strictly ascending frame order, so every clip has exactly one
//! encoding. The decoded clip passes every `fm_animation` bound
//! ([`fm_animation::MAX_CLIP_TRACKS`], [`fm_animation::MAX_TRACK_KEYS`],
//! [`fm_animation::MAX_CLIP_KEYS`], [`fm_animation::MAX_PHASE_FRAMES`],
//! property value ranges, bezier control points); the frame rate must be in
//! lowest terms.

use core::num::NonZeroU128;

use fm_animation::{
    AnimationPreset, Clip, CubicBezier, Easing, Keyframe, MAX_CLIP_KEYS, MAX_CLIP_TRACKS,
    MAX_PHASE_FRAMES, MAX_TRACK_KEYS, Phase, Property, Rgba, Timebase, Track, TrackKey, Value,
};
use fm_types::FrameRate;

use crate::{
    CodecError, MAX_TITLE_ANIMATION_BYTES, MAX_TITLE_PREVIEW_HEIGHT, MAX_TITLE_PREVIEW_PART_BYTES,
    MAX_TITLE_PREVIEW_PARTS, MAX_TITLE_PREVIEW_WIDTH, TitlePreviewSample, WireInputId,
    WireTitleClip, WireTitleElementId,
};

const CLIP: &str = "clip";
const BEZIER_TAG: u8 = 8;

/// Every easing with a fixed tag, in tag order; [`BEZIER_TAG`] is the gap.
const EASINGS: [Option<Easing>; 19] = [
    Some(Easing::Linear),
    Some(Easing::Step),
    Some(Easing::QuadIn),
    Some(Easing::QuadOut),
    Some(Easing::QuadInOut),
    Some(Easing::CubicIn),
    Some(Easing::CubicOut),
    Some(Easing::CubicInOut),
    None,
    Some(Easing::Hold),
    Some(Easing::SineIn),
    Some(Easing::SineOut),
    Some(Easing::SineInOut),
    Some(Easing::BackIn),
    Some(Easing::BackOut),
    Some(Easing::BackInOut),
    Some(Easing::BounceIn),
    Some(Easing::BounceOut),
    Some(Easing::BounceInOut),
];

fn invalid_clip(value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field: CLIP,
        value: value.into(),
    }
}

pub(super) fn push_varint(out: &mut Vec<u8>, mut value: u128) {
    loop {
        #[allow(clippy::cast_possible_truncation)]
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

pub(super) const fn zigzag(value: i64) -> u64 {
    #[allow(clippy::cast_sign_loss)]
    let mapped = ((value << 1) ^ (value >> 63)) as u64;
    mapped
}

const fn unzigzag(value: u64) -> i64 {
    #[allow(clippy::cast_possible_wrap)]
    let mapped = ((value >> 1) as i64) ^ -((value & 1) as i64);
    mapped
}

fn property_index(property: Property) -> u8 {
    let index = Property::ALL
        .iter()
        .position(|candidate| *candidate == property)
        .unwrap_or_default();
    u8::try_from(index).unwrap_or_default()
}

fn push_easing(out: &mut Vec<u8>, easing: Easing) {
    if let Easing::CubicBezier(curve) = easing {
        let (x1, y1, x2, y2) = curve.control_points();
        out.push(BEZIER_TAG);
        push_varint(out, u128::from(x1));
        push_varint(out, u128::from(zigzag(i64::from(y1))));
        push_varint(out, u128::from(x2));
        push_varint(out, u128::from(zigzag(i64::from(y2))));
        return;
    }
    let tag = EASINGS
        .iter()
        .position(|candidate| *candidate == Some(easing))
        .unwrap_or_default();
    out.push(u8::try_from(tag).unwrap_or_default());
}

/// The binary encoding of `clip` (see the module documentation).
fn clip_bytes(clip: &WireTitleClip) -> Vec<u8> {
    let mut out = Vec::new();
    let rate = clip.timebase().rate();
    push_varint(&mut out, u128::from(rate.numerator()));
    push_varint(&mut out, u128::from(rate.denominator()));
    push_varint(&mut out, u128::from(clip.in_frames()));
    push_varint(&mut out, u128::from(clip.out_frames()));
    push_varint(&mut out, clip.tracks().len() as u128);
    for track in clip.tracks() {
        push_varint(&mut out, track.key.target.get().get());
        out.push(match track.key.phase {
            Phase::In => 0,
            Phase::Out => 1,
        });
        out.push(property_index(track.key.property));
        push_varint(&mut out, track.keys.len() as u128);
        for key in &track.keys {
            push_varint(&mut out, u128::from(key.frame));
            match key.value {
                Value::Color(color) => out.extend_from_slice(&[color.r, color.g, color.b, color.a]),
                Value::Scalar(scalar) => push_varint(&mut out, u128::from(zigzag(scalar))),
            }
            push_easing(&mut out, key.easing);
        }
    }
    out
}

/// Encodes a clip as one base64url field value.
///
/// # Errors
///
/// [`CodecError::InvalidField`] naming `clip` when the clip fails
/// `fm_animation` validation or its encoding exceeds
/// [`MAX_TITLE_ANIMATION_BYTES`].
pub(super) fn encode_clip(clip: &WireTitleClip) -> Result<String, CodecError> {
    clip.validate()
        .map_err(|error| invalid_clip(error.to_string()))?;
    let bytes = clip_bytes(clip);
    if bytes.len() > MAX_TITLE_ANIMATION_BYTES {
        return Err(invalid_clip(format!("{} bytes", bytes.len())));
    }
    Ok(crate::monitor_image::base64url_encode(&bytes))
}

/// The size of `clip`'s binary encoding, in bytes; a clip whose size
/// exceeds [`MAX_TITLE_ANIMATION_BYTES`] cannot be carried.
#[must_use]
pub fn title_clip_wire_bytes(clip: &WireTitleClip) -> usize {
    clip_bytes(clip).len()
}

pub(super) struct Reader<'a> {
    pub(super) bytes: &'a [u8],
    pub(super) position: usize,
}

impl Reader<'_> {
    pub(super) fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.position)?;
        self.position += 1;
        Some(byte)
    }

    /// A minimal LEB128 varint of at most `bits` significant bits.
    pub(super) fn varint(&mut self, bits: u32) -> Option<u128> {
        let mut value = 0_u128;
        let mut shift = 0_u32;
        loop {
            let byte = self.byte()?;
            let payload = u128::from(byte & 0x7f);
            if shift >= bits || (shift > 0 && byte == 0) {
                // Past the width, or a trailing zero byte (overlong form).
                return None;
            }
            if shift + 7 > bits && payload >> (bits - shift) != 0 {
                return None;
            }
            value |= payload << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
        }
    }

    pub(super) fn u32(&mut self) -> Option<u32> {
        u32::try_from(self.varint(32)?).ok()
    }

    fn i64(&mut self) -> Option<i64> {
        Some(unzigzag(u64::try_from(self.varint(64)?).ok()?))
    }

    pub(super) fn i32(&mut self) -> Option<i32> {
        i32::try_from(self.i64()?).ok()
    }

    fn easing(&mut self) -> Option<Easing> {
        let tag = self.byte()?;
        if tag == BEZIER_TAG {
            let x1 = self.u32()?;
            let y1 = self.i32()?;
            let x2 = self.u32()?;
            let y2 = self.i32()?;
            return CubicBezier::new(x1, y1, x2, y2)
                .ok()
                .map(Easing::CubicBezier);
        }
        *EASINGS.get(usize::from(tag))?
    }

    fn track(&mut self, total_keys: &mut usize) -> Option<Track<WireTitleElementId>> {
        let element = WireTitleElementId::new(NonZeroU128::new(self.varint(128)?)?);
        let phase = match self.byte()? {
            0 => Phase::In,
            1 => Phase::Out,
            _ => return None,
        };
        let property = *Property::ALL.get(usize::from(self.byte()?))?;
        let count = usize::try_from(self.varint(32)?).ok()?;
        *total_keys = total_keys.checked_add(count)?;
        if count == 0 || count > MAX_TRACK_KEYS || *total_keys > MAX_CLIP_KEYS {
            return None;
        }
        let mut keys = Vec::with_capacity(count);
        for _ in 0..count {
            let frame = self.u32()?;
            let value = if property == Property::Color {
                let [r, g, b, a] = [self.byte()?, self.byte()?, self.byte()?, self.byte()?];
                Value::Color(Rgba::new(r, g, b, a))
            } else {
                Value::Scalar(self.i64()?)
            };
            keys.push(Keyframe::new(frame, value, self.easing()?));
        }
        Some(Track::new(TrackKey::new(element, phase, property), keys))
    }

    fn clip(&mut self) -> Option<WireTitleClip> {
        let numerator = self.u32()?;
        let denominator = self.u32()?;
        let rate = FrameRate::new(numerator, denominator)
            .ok()
            .filter(|rate| rate.numerator() == numerator && rate.denominator() == denominator)?;
        let in_frames = self.u32()?;
        let out_frames = self.u32()?;
        if in_frames > MAX_PHASE_FRAMES || out_frames > MAX_PHASE_FRAMES {
            return None;
        }
        let count = usize::try_from(self.varint(32)?).ok()?;
        if count > MAX_CLIP_TRACKS {
            return None;
        }
        let mut tracks: Vec<Track<WireTitleElementId>> = Vec::with_capacity(count);
        let mut total_keys = 0_usize;
        for _ in 0..count {
            let track = self.track(&mut total_keys)?;
            if tracks.last().is_some_and(|last| last.key >= track.key) {
                return None;
            }
            tracks.push(track);
        }
        if self.position != self.bytes.len() {
            return None;
        }
        Clip::from_tracks(Timebase::new(rate), in_frames, out_frames, tracks).ok()
    }
}

/// Strictly decodes one base64url clip field.
///
/// # Errors
///
/// [`CodecError::InvalidField`] naming `clip` for non-canonical base64url,
/// an oversized, truncated, non-minimal, or trailing encoding, an unordered
/// track, or a clip `fm_animation` refuses.
pub(super) fn decode_clip(value: &str) -> Result<WireTitleClip, CodecError> {
    let encoded_limit = (MAX_TITLE_ANIMATION_BYTES * 4).div_ceil(3);
    if value.len() > encoded_limit {
        return Err(invalid_clip(format!("{} characters", value.len())));
    }
    let bytes = crate::monitor_image::base64url_decode(value)
        .ok_or_else(|| invalid_clip(format!("{} characters", value.len())))?;
    Reader {
        bytes: &bytes,
        position: 0,
    }
    .clip()
    .ok_or_else(|| invalid_clip(format!("{} bytes", bytes.len())))
}

/// The wire token of an animation phase.
pub(super) const fn encode_animation_phase(phase: Phase) -> &'static str {
    match phase {
        Phase::In => "in",
        Phase::Out => "out",
    }
}

/// Strictly decodes `in` or `out`.
pub(super) fn decode_animation_phase(value: String) -> Result<Phase, CodecError> {
    match value.as_str() {
        "in" => Ok(Phase::In),
        "out" => Ok(Phase::Out),
        _ => Err(CodecError::InvalidField {
            field: "phase",
            value,
        }),
    }
}

/// Strictly decodes an [`AnimationPreset`] id.
pub(super) fn decode_animation_preset(value: String) -> Result<AnimationPreset, CodecError> {
    AnimationPreset::from_id(&value).ok_or(CodecError::InvalidField {
        field: "preset",
        value,
    })
}

/// Enforces `frames <= MAX_PHASE_FRAMES`.
pub(super) fn validate_phase_frames(frames: u32) -> Result<(), CodecError> {
    if frames <= MAX_PHASE_FRAMES {
        Ok(())
    } else {
        Err(CodecError::InvalidField {
            field: "frames",
            value: frames.to_string(),
        })
    }
}

/// The `phase` token and `frame` of a preview sample.
pub(super) const fn encode_preview_sample(sample: TitlePreviewSample) -> (&'static str, u32) {
    match sample {
        TitlePreviewSample::Static => ("static", 0),
        TitlePreviewSample::In { frame } => ("in", frame),
        TitlePreviewSample::Hold => ("hold", 0),
        TitlePreviewSample::Out { frame } => ("out", frame),
    }
}

/// Enforces the preview sample bounds: `frame` is zero for `static` and
/// `hold` and at most [`MAX_PHASE_FRAMES`] otherwise.
pub(super) fn validate_preview_sample(sample: TitlePreviewSample) -> Result<(), CodecError> {
    let (_, frame) = encode_preview_sample(sample);
    let valid = match sample {
        TitlePreviewSample::Static | TitlePreviewSample::Hold => frame == 0,
        TitlePreviewSample::In { .. } | TitlePreviewSample::Out { .. } => frame <= MAX_PHASE_FRAMES,
    };
    if valid {
        Ok(())
    } else {
        Err(CodecError::InvalidField {
            field: "frame",
            value: frame.to_string(),
        })
    }
}

/// Strictly decodes a preview sample from its `phase` token and `frame`.
pub(super) fn decode_preview_sample(
    phase: String,
    frame: u64,
) -> Result<TitlePreviewSample, CodecError> {
    let frame_value = u32::try_from(frame).map_err(|_| CodecError::InvalidField {
        field: "frame",
        value: frame.to_string(),
    })?;
    let sample = match phase.as_str() {
        "static" => TitlePreviewSample::Static,
        "in" => TitlePreviewSample::In { frame: frame_value },
        "hold" => TitlePreviewSample::Hold,
        "out" => TitlePreviewSample::Out { frame: frame_value },
        _ => {
            return Err(CodecError::InvalidField {
                field: "phase",
                value: phase,
            });
        }
    };
    if matches!(
        sample,
        TitlePreviewSample::Static | TitlePreviewSample::Hold
    ) && frame != 0
    {
        return Err(CodecError::InvalidField {
            field: "frame",
            value: frame.to_string(),
        });
    }
    validate_preview_sample(sample)?;
    Ok(sample)
}

/// Enforces `1..=640` x `1..=360`.
pub(super) fn validate_preview_size(width: u16, height: u16) -> Result<(), CodecError> {
    if !(1..=MAX_TITLE_PREVIEW_WIDTH).contains(&width) {
        return Err(CodecError::InvalidField {
            field: "width",
            value: width.to_string(),
        });
    }
    if !(1..=MAX_TITLE_PREVIEW_HEIGHT).contains(&height) {
        return Err(CodecError::InvalidField {
            field: "height",
            value: height.to_string(),
        });
    }
    Ok(())
}

/// Enforces the `title_preview_frame` part bounds.
pub(super) fn validate_preview_part(
    width: u16,
    height: u16,
    part: u16,
    parts: u16,
    data_len: usize,
) -> Result<(), CodecError> {
    validate_preview_size(width, height)?;
    if !(1..=MAX_TITLE_PREVIEW_PARTS).contains(&parts) {
        return Err(CodecError::InvalidField {
            field: "parts",
            value: parts.to_string(),
        });
    }
    if part >= parts {
        return Err(CodecError::InvalidField {
            field: "part",
            value: part.to_string(),
        });
    }
    if !(1..=MAX_TITLE_PREVIEW_PART_BYTES).contains(&data_len) {
        return Err(CodecError::InvalidField {
            field: "data",
            value: format!("{data_len} bytes"),
        });
    }
    Ok(())
}

/// Enforces a nonempty, strictly ascending `inputs` list.
pub(super) fn validate_animation_inputs(inputs: &[WireInputId]) -> Result<(), CodecError> {
    let ascending = inputs.windows(2).all(|pair| pair[0].get() < pair[1].get());
    if inputs.is_empty() || !ascending {
        return Err(CodecError::InvalidField {
            field: "inputs",
            value: inputs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        });
    }
    Ok(())
}
