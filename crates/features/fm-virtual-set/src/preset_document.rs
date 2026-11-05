//! Canonical text for a virtual-set preset store (IN-021).
//!
//! The document carries every named camera pose and its recall move in
//! ascending preset-id order, and is strictly decoded through the same
//! constructors that validate a live store.

use core::fmt;
use core::num::NonZeroU128;

use crate::{
    CameraPose, EasingCurve, EasingPoint, MAX_EASING_POINTS, PlanError, PresetId, PresetStore,
    VirtualSetPreset,
};

/// Why a virtual-set preset document could not be restored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PresetDocumentError {
    /// The first line is not the current document header.
    Header,
    /// A preset line is missing, malformed, or has the wrong field count.
    Preset,
    /// Preset lines are not in strictly ascending id order.
    Order,
    /// The preset failed camera, easing, or store validation.
    Plan(PlanError),
}

impl fmt::Display for PresetDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header => formatter.write_str("virtual-set preset document header is unknown"),
            Self::Preset => formatter.write_str("virtual-set preset line is malformed"),
            Self::Order => formatter.write_str("virtual-set presets are not in ascending id order"),
            Self::Plan(error) => write!(formatter, "virtual-set preset is invalid: {error}"),
        }
    }
}

impl std::error::Error for PresetDocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Plan(error) => Some(error),
            _ => None,
        }
    }
}

impl PresetStore {
    /// Encodes the store as canonical text:
    ///
    /// ```text
    /// freemix-virtual-set-presets 1
    /// preset <id> <name hex> <pan x> <pan y> <zoom> <move frames> <points> <t> <value>...
    /// ```
    #[must_use]
    pub fn to_document(&self) -> String {
        use fmt::Write as _;
        let mut output = String::from("freemix-virtual-set-presets 1\n");
        for preset in self.iter() {
            let pose = preset.pose();
            write!(
                output,
                "preset {} {} {} {} {} {} {}",
                preset.id(),
                hex_text(preset.name()),
                pose.pan_x_permille(),
                pose.pan_y_permille(),
                pose.zoom_permille(),
                preset.move_frames(),
                preset.easing().points().len(),
            )
            .expect("writing to a String cannot fail");
            for point in preset.easing().points() {
                write!(output, " {} {}", point.t_permille, point.value_permille)
                    .expect("writing to a String cannot fail");
            }
            output.push('\n');
        }
        output
    }

    /// Restores a store from `document`.
    ///
    /// # Errors
    ///
    /// Returns [`PresetDocumentError`] for anything but the exact contract.
    pub fn from_document(document: &str) -> Result<Self, PresetDocumentError> {
        let mut lines = document.lines();
        if lines.next() != Some("freemix-virtual-set-presets 1") {
            return Err(PresetDocumentError::Header);
        }
        let mut store = Self::new();
        let mut previous = 0;
        for line in lines {
            let rest = line
                .strip_prefix("preset ")
                .ok_or(PresetDocumentError::Preset)?;
            let parts: Vec<&str> = rest.split(' ').collect();
            let Some((id, parts)) = parts.split_first() else {
                return Err(PresetDocumentError::Preset);
            };
            let id = id
                .parse::<u128>()
                .ok()
                .filter(|value| *value != 0)
                .ok_or(PresetDocumentError::Preset)?;
            if id <= previous {
                return Err(PresetDocumentError::Order);
            }
            previous = id;
            let Some((name, parts)) = parts.split_first() else {
                return Err(PresetDocumentError::Preset);
            };
            let name = unhex_text(name).ok_or(PresetDocumentError::Preset)?;
            if parts.len() < 5 {
                return Err(PresetDocumentError::Preset);
            }
            let pan_x = parts[0]
                .parse::<u32>()
                .map_err(|_| PresetDocumentError::Preset)?;
            let pan_y = parts[1]
                .parse::<u32>()
                .map_err(|_| PresetDocumentError::Preset)?;
            let zoom = parts[2]
                .parse::<u32>()
                .map_err(|_| PresetDocumentError::Preset)?;
            let move_frames = parts[3]
                .parse::<u32>()
                .map_err(|_| PresetDocumentError::Preset)?;
            let count = parts[4]
                .parse::<usize>()
                .map_err(|_| PresetDocumentError::Preset)?;
            if !(2..=MAX_EASING_POINTS).contains(&count)
                || parts.len() != 5 + count.saturating_mul(2)
            {
                return Err(PresetDocumentError::Preset);
            }
            let points = parts[5..]
                .chunks(2)
                .map(|pair| {
                    Ok(EasingPoint::new(
                        pair[0].parse().map_err(|_| PresetDocumentError::Preset)?,
                        pair[1].parse().map_err(|_| PresetDocumentError::Preset)?,
                    ))
                })
                .collect::<Result<Vec<_>, PresetDocumentError>>()?;
            let pose = CameraPose::new(pan_x, pan_y, zoom).map_err(PresetDocumentError::Plan)?;
            let easing = EasingCurve::new(points).map_err(PresetDocumentError::Plan)?;
            let preset = VirtualSetPreset::new(
                PresetId::new(NonZeroU128::new(id).ok_or(PresetDocumentError::Preset)?),
                name,
                pose,
                move_frames,
                easing,
            )
            .map_err(PresetDocumentError::Plan)?;
            store.insert(preset).map_err(PresetDocumentError::Plan)?;
        }
        if store.is_empty() {
            return Err(PresetDocumentError::Preset);
        }
        Ok(store)
    }
}

fn hex_text(text: &str) -> String {
    use fmt::Write as _;
    let mut output = String::with_capacity(text.len() * 2);
    for byte in text.bytes() {
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn unhex_text(text: &str) -> Option<String> {
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return None;
    }
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        output.push((digit(pair[0])? << 4) | digit(pair[1])?);
    }
    String::from_utf8(output).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> PresetStore {
        let mut store = PresetStore::new();
        store
            .insert(
                VirtualSetPreset::new(
                    PresetId::new(NonZeroU128::new(1).unwrap()),
                    "Wide",
                    CameraPose::wide(),
                    0,
                    EasingCurve::linear(),
                )
                .unwrap(),
            )
            .unwrap();
        store
            .insert(
                VirtualSetPreset::new(
                    PresetId::new(NonZeroU128::new(2).unwrap()),
                    "Close up",
                    CameraPose::new(250, 500, 2_000).unwrap(),
                    60,
                    EasingCurve::ease_in_out(),
                )
                .unwrap(),
            )
            .unwrap();
        store
    }

    #[test]
    fn preset_documents_round_trip() {
        let store = store();
        let document = store.to_document();
        assert_eq!(
            document,
            "freemix-virtual-set-presets 1\n\
             preset 1 57696465 500 500 1000 0 2 0 0 1000 1000\n\
             preset 2 436c6f7365207570 250 500 2000 60 5 0 0 250 100 500 500 750 900 1000 1000\n"
        );
        let restored = PresetStore::from_document(&document).unwrap();
        assert_eq!(restored, store);
        assert_eq!(restored.to_document(), document);
    }

    #[test]
    fn preset_documents_are_strict() {
        for document in [
            "",
            "freemix-virtual-set-presets 2\n",
            "freemix-virtual-set-presets 1\n",
            "freemix-virtual-set-presets 1\npreset 0 57 500 500 1000 0 2 0 0 1000 1000\n",
            "freemix-virtual-set-presets 1\npreset 1 zz 500 500 1000 0 2 0 0 1000 1000\n",
            "freemix-virtual-set-presets 1\npreset 1 57 1001 500 1000 0 2 0 0 1000 1000\n",
            "freemix-virtual-set-presets 1\npreset 1 57 500 500 999 0 2 0 0 1000 1000\n",
            "freemix-virtual-set-presets 1\npreset 1 57 500 500 1000 0 3 0 0 1000 1000\n",
            "freemix-virtual-set-presets 1\npreset 1 57 500 500 1000 0 2 0 0 1000\n",
            "freemix-virtual-set-presets 1\npreset 1 57 500 500 1000 0 2 0 0 1000 1000 extra\n",
            "freemix-virtual-set-presets 1\npreset 1 57 500 500 1000 0 2 0 0 1000 1000\npreset 1 57 500 500 1000 0 2 0 0 1000 1000\n",
            "freemix-virtual-set-presets 1\npreset 2 57 500 500 1000 0 2 0 0 1000 1000\npreset 1 57 500 500 1000 0 2 0 0 1000 1000\n",
            "freemix-virtual-set-presets 1\npreset 1 57 500 500 1000 0 2 0 1000 1000 1000\n",
            "freemix-virtual-set-presets 1\npreset 1 57 500 500 1000 0 2 0 0 500 400\n",
            "freemix-virtual-set-presets 1\nextra\n",
        ] {
            assert!(
                PresetStore::from_document(document).is_err(),
                "{document:?}"
            );
        }
    }
}
