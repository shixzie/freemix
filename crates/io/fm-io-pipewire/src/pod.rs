//! The few SPA pod types a raw video stream negotiates with, encoded and
//! decoded in safe Rust.
//!
//! A pod is a native-endian `u32` body size and `u32` type, then the body,
//! padded to 8 bytes. An object is its type and parameter id, then
//! properties (a `u32` key, `u32` flags, and a padded pod each). A choice is
//! its kind, flags, and child header (element size and type), then the
//! packed element bodies. Only the types below are understood; anything else
//! decodes as [`Value::Other`]. The encoder's output is checked byte for
//! byte against libspa's own builder in the tests.

/// SPA pod types (`enum spa_type`).
pub mod types {
    pub const NONE: u32 = 1;
    pub const ID: u32 = 3;
    pub const INT: u32 = 4;
    pub const RECTANGLE: u32 = 10;
    pub const FRACTION: u32 = 11;
    pub const OBJECT: u32 = 15;
    pub const CHOICE: u32 = 19;
    /// `SPA_TYPE_OBJECT_Format`.
    pub const OBJECT_FORMAT: u32 = 0x4_0003;
    /// `SPA_TYPE_OBJECT_ParamBuffers`.
    pub const OBJECT_PARAM_BUFFERS: u32 = 0x4_0004;
}

/// Parameter ids (`enum spa_param_type`).
pub mod param {
    pub const ENUM_FORMAT: u32 = 3;
    pub const FORMAT: u32 = 4;
    pub const BUFFERS: u32 = 5;
    /// `SPA_PARAM_BUFFERS_dataType`: a flags choice of `1 << SPA_DATA_*`.
    pub const BUFFERS_DATA_TYPE: u32 = 6;
}

/// Format property keys (`enum spa_format`).
pub mod format {
    pub const MEDIA_TYPE: u32 = 1;
    pub const MEDIA_SUBTYPE: u32 = 2;
    pub const VIDEO_FORMAT: u32 = 0x2_0001;
    pub const VIDEO_SIZE: u32 = 0x2_0003;
    pub const VIDEO_FRAMERATE: u32 = 0x2_0004;
    /// `SPA_MEDIA_TYPE_video`.
    pub const MEDIA_VIDEO: u32 = 2;
    /// `SPA_MEDIA_SUBTYPE_raw`.
    pub const SUBTYPE_RAW: u32 = 1;
}

/// The 4-byte raw video formats (`enum spa_video_format`).
pub mod video {
    pub const RGBX: u32 = 7;
    pub const BGRX: u32 = 8;
    pub const XRGB: u32 = 9;
    pub const XBGR: u32 = 10;
    pub const RGBA: u32 = 11;
    pub const BGRA: u32 = 12;
    pub const ARGB: u32 = 13;
    pub const ABGR: u32 = 14;
}

/// Buffer data types (`enum spa_data_type`).
pub mod data {
    pub const MEM_PTR: u32 = 1;
    pub const MEM_FD: u32 = 2;
}

/// A choice's kind (`enum spa_choice_type`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChoiceKind {
    /// No choice: the first value is the value.
    None,
    /// Default, minimum, maximum.
    Range,
    /// Default, minimum, maximum, step.
    Step,
    /// Default, then the alternatives.
    Enum,
    /// Default, then the possible flags.
    Flags,
}

impl ChoiceKind {
    const fn raw(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Range => 1,
            Self::Step => 2,
            Self::Enum => 3,
            Self::Flags => 4,
        }
    }

    const fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            0 => Self::None,
            1 => Self::Range,
            2 => Self::Step,
            3 => Self::Enum,
            4 => Self::Flags,
            _ => return None,
        })
    }
}

/// A pod value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    Id(u32),
    Int(i32),
    Rectangle {
        width: u32,
        height: u32,
    },
    Fraction {
        num: u32,
        denom: u32,
    },
    /// Values of one element type. An empty choice cannot be encoded.
    Choice {
        kind: ChoiceKind,
        values: Vec<Value>,
    },
    Object(Object),
    /// A pod of a type not understood here, by its type.
    Other(u32),
}

/// An object pod: its type, parameter id, and `(key, value)` properties.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Object {
    pub type_: u32,
    pub id: u32,
    pub properties: Vec<(u32, Value)>,
}

impl Object {
    /// The value of property `key`, if present.
    #[must_use]
    pub fn get(&self, key: u32) -> Option<&Value> {
        self.properties
            .iter()
            .find(|(candidate, _)| *candidate == key)
            .map(|(_, value)| value)
    }
}

const fn pad8(size: usize) -> usize {
    (size + 7) & !7
}

fn push(out: &mut Vec<u8>, word: u32) {
    out.extend_from_slice(&word.to_ne_bytes());
}

impl Value {
    /// The pod type and body of a value; `None` for what cannot be
    /// encoded (an empty or mixed choice, [`Value::Other`]).
    fn body(&self) -> Option<(u32, Vec<u8>)> {
        let mut body = Vec::new();
        let type_ = match self {
            Self::Id(id) => {
                push(&mut body, *id);
                types::ID
            }
            Self::Int(value) => {
                body.extend_from_slice(&value.to_ne_bytes());
                types::INT
            }
            Self::Rectangle { width, height } => {
                push(&mut body, *width);
                push(&mut body, *height);
                types::RECTANGLE
            }
            Self::Fraction { num, denom } => {
                push(&mut body, *num);
                push(&mut body, *denom);
                types::FRACTION
            }
            Self::Choice { kind, values } => {
                let mut elements = Vec::with_capacity(values.len());
                for value in values {
                    if matches!(value, Self::Choice { .. } | Self::Object(_)) {
                        return None;
                    }
                    elements.push(value.body()?);
                }
                let (child_type, first) = elements.first()?;
                let child_size = first.len();
                if elements
                    .iter()
                    .any(|(type_, bytes)| type_ != child_type || bytes.len() != child_size)
                {
                    return None;
                }
                push(&mut body, kind.raw());
                push(&mut body, 0);
                push(&mut body, u32::try_from(child_size).ok()?);
                push(&mut body, *child_type);
                for (_, bytes) in elements {
                    body.extend_from_slice(&bytes);
                }
                types::CHOICE
            }
            Self::Object(object) => {
                push(&mut body, object.type_);
                push(&mut body, object.id);
                for (key, value) in &object.properties {
                    push(&mut body, *key);
                    push(&mut body, 0);
                    body.extend_from_slice(&value.encode()?);
                }
                types::OBJECT
            }
            Self::Other(_) => return None,
        };
        Some((type_, body))
    }

    /// The value as a pod, padded to 8 bytes; `None` for what cannot be
    /// encoded (an empty or mixed choice, [`Value::Other`]).
    #[must_use]
    pub fn encode(&self) -> Option<Vec<u8>> {
        let (type_, body) = self.body()?;
        let mut pod = Vec::with_capacity(8 + pad8(body.len()));
        push(&mut pod, u32::try_from(body.len()).ok()?);
        push(&mut pod, type_);
        pod.extend_from_slice(&body);
        pod.resize(8 + pad8(body.len()), 0);
        Some(pod)
    }

    /// Decodes the pod at the start of `bytes`; `None` when it is
    /// truncated or malformed. Bytes past the pod are ignored.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let size = usize::try_from(word(bytes, 0)?).ok()?;
        let type_ = word(bytes, 4)?;
        let body = bytes.get(8..8usize.checked_add(size)?)?;
        decode_body(type_, body)
    }

    /// The value, or a choice's first (current or default) value.
    #[must_use]
    pub fn current(&self) -> Option<&Self> {
        match self {
            Self::Choice { values, .. } => values.first(),
            value => Some(value),
        }
    }
}

fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let bytes = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_ne_bytes(bytes.try_into().ok()?))
}

fn decode_body(type_: u32, body: &[u8]) -> Option<Value> {
    Some(match type_ {
        types::ID => Value::Id(word(body, 0)?),
        types::INT => Value::Int(i32::from_ne_bytes(word(body, 0)?.to_ne_bytes())),
        types::RECTANGLE => Value::Rectangle {
            width: word(body, 0)?,
            height: word(body, 4)?,
        },
        types::FRACTION => Value::Fraction {
            num: word(body, 0)?,
            denom: word(body, 4)?,
        },
        types::CHOICE => {
            let kind = ChoiceKind::from_raw(word(body, 0)?)?;
            let child_size = usize::try_from(word(body, 8)?).ok()?;
            let child_type = word(body, 12)?;
            let elements = body.get(16..)?;
            if child_size == 0 || child_type == types::CHOICE || child_type == types::OBJECT {
                return None;
            }
            let values = elements
                .chunks_exact(child_size)
                .map(|element| decode_body(child_type, element))
                .collect::<Option<Vec<_>>>()?;
            if values.is_empty() {
                return None;
            }
            Value::Choice { kind, values }
        }
        types::OBJECT => {
            let mut properties = Vec::new();
            let mut at = 8;
            while at < body.len() {
                let key = word(body, at)?;
                let value_size = usize::try_from(word(body, at + 8)?).ok()?;
                let value = body.get(at + 8..)?;
                properties.push((key, Value::decode(value)?));
                // The last property's padding may be left off.
                at = at
                    .checked_add(16)?
                    .checked_add(pad8(value_size))?
                    .min(body.len());
            }
            Value::Object(Object {
                type_: word(body, 0)?,
                id: word(body, 4)?,
                properties,
            })
        }
        other => Value::Other(other),
    })
}

/// The `EnumFormat` a raw video input stream offers: any of `formats` (the
/// first preferred), sizes from 1x1 to 16384x16384 (1920x1080 preferred),
/// and any rate up to 1000/1 (30/1 preferred). No DMA-BUF modifiers are
/// offered, so a compositor copies into shared memory.
///
/// # Panics
///
/// Never: the offer always encodes, `formats` empty or not.
#[must_use]
pub fn video_offer(formats: &[u32]) -> Vec<u8> {
    let formats = if formats.is_empty() {
        &[video::BGRX][..]
    } else {
        formats
    };
    // An enum choice's first value is its default.
    let choices: Vec<Value> = formats[..1]
        .iter()
        .chain(formats)
        .map(|format| Value::Id(*format))
        .collect();
    Value::Object(Object {
        type_: types::OBJECT_FORMAT,
        id: param::ENUM_FORMAT,
        properties: vec![
            (format::MEDIA_TYPE, Value::Id(format::MEDIA_VIDEO)),
            (format::MEDIA_SUBTYPE, Value::Id(format::SUBTYPE_RAW)),
            (
                format::VIDEO_FORMAT,
                Value::Choice {
                    kind: ChoiceKind::Enum,
                    values: choices,
                },
            ),
            (
                format::VIDEO_SIZE,
                Value::Choice {
                    kind: ChoiceKind::Range,
                    values: vec![
                        Value::Rectangle {
                            width: 1920,
                            height: 1080,
                        },
                        Value::Rectangle {
                            width: 1,
                            height: 1,
                        },
                        Value::Rectangle {
                            width: 16_384,
                            height: 16_384,
                        },
                    ],
                },
            ),
            (
                format::VIDEO_FRAMERATE,
                Value::Choice {
                    kind: ChoiceKind::Range,
                    values: vec![
                        Value::Fraction { num: 30, denom: 1 },
                        Value::Fraction { num: 0, denom: 1 },
                        Value::Fraction {
                            num: 1000,
                            denom: 1,
                        },
                    ],
                },
            ),
        ],
    })
    .encode()
    .expect("a video offer always encodes")
}

/// The `Buffers` parameter asking for shared memory buffers only (pointer
/// or file descriptor memory, which the stream maps).
///
/// # Panics
///
/// Never: the parameter always encodes.
#[must_use]
pub fn shared_memory_buffers() -> Vec<u8> {
    let mask = (1 << data::MEM_PTR) | (1 << data::MEM_FD);
    Value::Object(Object {
        type_: types::OBJECT_PARAM_BUFFERS,
        id: param::BUFFERS,
        properties: vec![(
            param::BUFFERS_DATA_TYPE,
            Value::Choice {
                kind: ChoiceKind::Flags,
                values: vec![Value::Int(mask)],
            },
        )],
    })
    .encode()
    .expect("a buffers parameter always encodes")
}

/// A negotiated raw video format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawVideo {
    /// The `spa_video_format`.
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

/// The raw video format a `Format` pod fixes (a choice's default where a
/// value is left a choice); `None` when the pod is not a raw video
/// `Format` or lacks its format or size.
#[must_use]
pub fn raw_video(pod: &[u8]) -> Option<RawVideo> {
    let Value::Object(object) = Value::decode(pod)? else {
        return None;
    };
    if object.type_ != types::OBJECT_FORMAT || object.id != param::FORMAT {
        return None;
    }
    let id = |key| match object.get(key)?.current()? {
        Value::Id(id) => Some(*id),
        _ => None,
    };
    if id(format::MEDIA_TYPE)? != format::MEDIA_VIDEO
        || id(format::MEDIA_SUBTYPE)? != format::SUBTYPE_RAW
    {
        return None;
    }
    let Value::Rectangle { width, height } = object.get(format::VIDEO_SIZE)?.current()? else {
        return None;
    };
    Some(RawVideo {
        format: id(format::VIDEO_FORMAT)?,
        width: *width,
        height: *height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_ne_bytes()).collect()
    }

    /// libspa's `spa_pod_builder_add_object` of the offer (`PipeWire` 1.0.5
    /// headers): format choice `BGRx` then the eight 4-byte formats.
    const OFFER: [u32; 62] = [
        0xf0, 0xf, 0x40003, 0x3, 0x1, 0x0, 0x4, 0x3, 0x2, 0x0, 0x2, 0x0, 0x4, 0x3, 0x1, 0x0,
        0x20001, 0x0, 0x34, 0x13, 0x3, 0x0, 0x4, 0x3, 0x8, 0x8, 0x7, 0x9, 0xa, 0xc, 0xb, 0xd, 0xe,
        0x0, 0x20003, 0x0, 0x28, 0x13, 0x1, 0x0, 0x8, 0xa, 0x780, 0x438, 0x1, 0x1, 0x4000, 0x4000,
        0x20004, 0x0, 0x28, 0x13, 0x1, 0x0, 0x8, 0xb, 0x1e, 0x1, 0x0, 0x1, 0x3e8, 0x1,
    ];

    /// libspa's fixed `Format` of a 640x360 `BGRx` share with a variable
    /// rate up to 30/1 (as xdg-desktop-portal-wlr fixes it).
    const FORMAT: [u32; 48] = [
        0xb8, 0xf, 0x40003, 0x4, 0x1, 0x0, 0x4, 0x3, 0x2, 0x0, 0x2, 0x0, 0x4, 0x3, 0x1, 0x0,
        0x20001, 0x0, 0x4, 0x3, 0x8, 0x0, 0x20003, 0x0, 0x8, 0xa, 0x280, 0x168, 0x20004, 0x0, 0x8,
        0xb, 0x0, 0x1, 0x20005, 0x0, 0x28, 0x13, 0x1, 0x0, 0x8, 0xb, 0x1e, 0x1, 0x1, 0x1, 0x1e,
        0x1,
    ];

    /// libspa's `Buffers` with `dataType` a flags choice of `MemPtr` and
    /// `MemFd`.
    const BUFFERS: [u32; 14] = [
        0x30, 0xf, 0x40004, 0x5, 0x6, 0x0, 0x14, 0x13, 0x4, 0x0, 0x4, 0x4, 0x6, 0x0,
    ];

    const FORMATS: [u32; 8] = [
        video::BGRX,
        video::RGBX,
        video::XRGB,
        video::XBGR,
        video::BGRA,
        video::RGBA,
        video::ARGB,
        video::ABGR,
    ];

    #[test]
    fn the_offer_matches_libspa() {
        assert_eq!(video_offer(&FORMATS), words(&OFFER));
    }

    #[test]
    fn the_buffers_parameter_matches_libspa() {
        assert_eq!(shared_memory_buffers(), words(&BUFFERS));
    }

    #[test]
    fn a_fixed_format_is_read() {
        assert_eq!(
            raw_video(&words(&FORMAT)),
            Some(RawVideo {
                format: video::BGRX,
                width: 640,
                height: 360
            })
        );
        // Decoding and encoding again gives libspa's bytes back.
        let value = Value::decode(&words(&FORMAT)).unwrap();
        assert_eq!(value.encode(), Some(words(&FORMAT)));
        let value = Value::decode(&words(&OFFER)).unwrap();
        assert_eq!(value.encode(), Some(words(&OFFER)));
    }

    #[test]
    fn a_format_in_choices_reads_their_current_values() {
        // The offer itself: every value a choice, the first the default.
        let mut offer = words(&OFFER);
        assert_eq!(raw_video(&offer), None, "an EnumFormat is not a Format");
        offer[12..16].copy_from_slice(&param::FORMAT.to_ne_bytes());
        assert_eq!(
            raw_video(&offer),
            Some(RawVideo {
                format: video::BGRX,
                width: 1920,
                height: 1080
            })
        );
        // Without the padding of its last property.
        let mut trimmed = words(&BUFFERS);
        trimmed.truncate(52);
        trimmed[0..4].copy_from_slice(&0x2cu32.to_ne_bytes());
        let Some(Value::Object(object)) = Value::decode(&trimmed) else {
            panic!("{trimmed:?}");
        };
        assert_eq!(
            object.get(param::BUFFERS_DATA_TYPE),
            Some(&Value::Choice {
                kind: ChoiceKind::Flags,
                values: vec![Value::Int(6)]
            })
        );
    }

    #[test]
    fn other_formats_are_not_raw_video() {
        // Audio.
        let mut audio = words(&FORMAT);
        audio[32..36].copy_from_slice(&1u32.to_ne_bytes());
        assert_eq!(raw_video(&audio), None);
        // Encoded video.
        let mut encoded = words(&FORMAT);
        encoded[56..60].copy_from_slice(&2u32.to_ne_bytes());
        assert_eq!(raw_video(&encoded), None);
        // A size that is not a rectangle.
        let mut sizeless = words(&FORMAT);
        sizeless[100..104].copy_from_slice(&types::INT.to_ne_bytes());
        assert_eq!(raw_video(&sizeless), None);
    }

    #[test]
    fn truncated_or_corrupt_pods_never_decode_wrongly() {
        for pod in [words(&OFFER), words(&FORMAT), words(&BUFFERS)] {
            for length in 0..pod.len() {
                // A shorter buffer than the header says is refused.
                assert_eq!(Value::decode(&pod[..length]), None, "{length}");
            }
            for at in (0..pod.len()).step_by(4) {
                // Any word set to garbage decodes to something or nothing,
                // without panicking.
                for garbage in [0, 3, 0xffff_fff8, u32::MAX] {
                    let mut corrupt = pod.clone();
                    corrupt[at..at + 4].copy_from_slice(&garbage.to_ne_bytes());
                    let _ = Value::decode(&corrupt);
                    let _ = raw_video(&corrupt);
                }
            }
        }
    }

    #[test]
    fn unencodable_values_are_refused() {
        assert_eq!(Value::Other(99).encode(), None);
        let empty = Value::Choice {
            kind: ChoiceKind::Enum,
            values: Vec::new(),
        };
        assert_eq!(empty.encode(), None);
        let mixed = Value::Choice {
            kind: ChoiceKind::Enum,
            values: vec![
                Value::Id(1),
                Value::Rectangle {
                    width: 1,
                    height: 1,
                },
            ],
        };
        assert_eq!(mixed.encode(), None);
        assert_eq!(video_offer(&[]), video_offer(&[video::BGRX]));
    }
}
