//! The designer's edits as offline scene layer edit commands.
//!
//! Each [`SceneLayerEdit`] is exactly one of the project's scene layer
//! operations (`fm_model::Project::move_scene_layer`,
//! `set_scene_layer_geometry`, `set_scene_layer_appearance`,
//! `set_scene_layer_crop`, `set_scene_layer_mask`,
//! `set_scene_layer_z_order`), addressed by zero-based layer index as
//! those operations and their `freemix-cli scene-layer-*` commands are. A
//! running server takes the designer's edits live (by layer id); these
//! commands are the secondary export, a `freemix-cli` script
//! ([`SceneLayerEdit::cli_command`]) that reproduces a layout in a project
//! bundle.

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
    /// Sets the layer's drawing order key (`scene-layer-z-order`).
    ZOrder { index: usize, z_order: i32 },
}

/// A clockwise angle in millidegrees as degrees with at most three
/// decimals and no trailing zeros (`90`, `-12.5`).
fn degrees_text(millidegrees: i64) -> String {
    let sign = if millidegrees < 0 { "-" } else { "" };
    let magnitude = millidegrees.unsigned_abs();
    let (whole, fraction) = (magnitude / 1_000, magnitude % 1_000);
    if fraction == 0 {
        format!("{sign}{whole}")
    } else {
        let digits = format!("{fraction:03}");
        format!("{sign}{whole}.{}", digits.trim_end_matches('0'))
    }
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
            Self::Geometry { index, placement } => ("scene-layer-geometry", {
                let mut arguments = vec![
                    index.to_string(),
                    placement.x.to_string(),
                    placement.y.to_string(),
                    placement.width.to_string(),
                    placement.height.to_string(),
                    degrees_text(placement.rotation_millidegrees()),
                ];
                if (placement.anchor_x, placement.anchor_y)
                    != (
                        super::geometry::ANCHOR_CENTER,
                        super::geometry::ANCHOR_CENTER,
                    )
                {
                    arguments.extend([
                        "--anchor".to_owned(),
                        placement.anchor_x.to_string(),
                        placement.anchor_y.to_string(),
                    ]);
                }
                arguments
            }),
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
            Self::ZOrder { index, z_order } => (
                "scene-layer-z-order",
                vec![index.to_string(), z_order.to_string()],
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
