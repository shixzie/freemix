//! The designer's edits as the existing scene layer edit commands.
//!
//! Each [`SceneLayerEdit`] is exactly one of the project's scene layer
//! operations (`fm_model::Project::move_scene_layer`,
//! `set_scene_layer_geometry`, `set_scene_layer_appearance`,
//! `set_scene_layer_crop`, `set_scene_layer_mask`), addressed by zero-based
//! layer index as those operations and their `freemix-cli scene-layer-*`
//! commands are. The protocol has no live counterpart yet (scene layouts
//! change only between snapshots), so the designer hands a draft over as
//! a `freemix-cli` script ([`SceneLayerEdit::cli_command`]).

use fm_protocol::WireSceneId;

use super::geometry::{LayerPlacement, MaskRect};

/// One scene layer edit command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneLayerEdit {
    /// Moves the layer at `from` to stacking position `to` (`scene-layer-move`).
    Move { from: usize, to: usize },
    /// Sets the layer's placement (`scene-layer-geometry`).
    Geometry {
        index: usize,
        placement: LayerPlacement,
    },
    /// Sets the layer's enabled flag and opacity (`scene-layer-appearance`).
    Appearance {
        index: usize,
        enabled: bool,
        opacity: u8,
    },
    /// Sets (`x, y, width, height` in source pixels) or clears the crop
    /// (`scene-layer-crop`, `scene-layer-crop-clear`).
    Crop {
        index: usize,
        rect: Option<(u32, u32, u32, u32)>,
    },
    /// Sets or clears the mask (`scene-layer-mask`, `scene-layer-mask-clear`).
    Mask {
        index: usize,
        mask: Option<MaskRect>,
    },
}

/// `value` quoted for a POSIX shell when it needs it.
fn shell_word(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,".contains(c));
    if plain {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

impl SceneLayerEdit {
    /// The `freemix-cli` subcommand and its arguments after the project
    /// path and scene id.
    #[must_use]
    pub fn cli_arguments(&self) -> (&'static str, Vec<String>) {
        match *self {
            Self::Move { from, to } => ("scene-layer-move", vec![from.to_string(), to.to_string()]),
            Self::Geometry { index, placement } => (
                "scene-layer-geometry",
                vec![
                    index.to_string(),
                    placement.x.to_string(),
                    placement.y.to_string(),
                    placement.width.to_string(),
                    placement.height.to_string(),
                    (u16::from(placement.quarter_turns % 4) * 90).to_string(),
                ],
            ),
            Self::Appearance {
                index,
                enabled,
                opacity,
            } => (
                "scene-layer-appearance",
                vec![
                    index.to_string(),
                    if enabled { "on" } else { "off" }.to_owned(),
                    opacity.to_string(),
                ],
            ),
            Self::Crop { index, rect: None } => ("scene-layer-crop-clear", vec![index.to_string()]),
            Self::Crop {
                index,
                rect: Some((x, y, width, height)),
            } => (
                "scene-layer-crop",
                vec![
                    index.to_string(),
                    x.to_string(),
                    y.to_string(),
                    width.to_string(),
                    height.to_string(),
                ],
            ),
            Self::Mask { index, mask: None } => ("scene-layer-mask-clear", vec![index.to_string()]),
            Self::Mask {
                index,
                mask: Some(mask),
            } => (
                "scene-layer-mask",
                vec![
                    index.to_string(),
                    mask.x.to_string(),
                    mask.y.to_string(),
                    mask.width.to_string(),
                    mask.height.to_string(),
                    if mask.inverted { "inverted" } else { "normal" }.to_owned(),
                ],
            ),
        }
    }

    /// The edit as one `freemix-cli` command line on the project bundle at
    /// `project`.
    #[must_use]
    pub fn cli_command(&self, project: &str, scene: WireSceneId) -> String {
        let (command, arguments) = self.cli_arguments();
        let mut words = vec![
            "freemix-cli".to_owned(),
            command.to_owned(),
            shell_word(project),
            scene.to_string(),
        ];
        words.extend(arguments);
        words.join(" ")
    }
}
