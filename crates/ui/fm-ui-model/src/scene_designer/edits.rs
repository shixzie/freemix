//! The designer's edits as offline scene layer edit commands.
//!
//! Each [`SceneLayerEdit`] is exactly one of the project's scene layer
//! operations (`fm_model::Project::move_scene_layer`,
//! `set_scene_layer_geometry`, `set_scene_layer_appearance`,
//! `set_scene_layer_crop`, `set_scene_layer_mask`,
//! `set_scene_layer_key`, the effect list edits,
//! `set_scene_layer_z_order`), addressed by zero-based layer index as
//! those operations and their `freemix-cli scene-layer-*` commands are. A
//! running server takes the designer's edits live (by layer id); these
//! commands are the secondary export, a `freemix-cli` script
//! ([`SceneLayerEdit::cli_command`]) that reproduces a layout in a project
//! bundle.

use fm_protocol::{WireLayerEffect, WireLayerKey, WireSceneId};

use super::geometry::{LayerPlacement, MaskRect};

/// One scene layer edit command.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    /// Sets or clears the chroma or luma key (`scene-layer-key`).
    Key {
        index: usize,
        key: Option<WireLayerKey>,
    },
    /// Removes every effect of the layer (`scene-layer-effect-clear`).
    ClearEffects { index: usize },
    /// Appends one effect to the layer's list (`scene-layer-effect-add`).
    AddEffect {
        index: usize,
        effect: WireLayerEffect,
    },
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

/// The `scene-layer-key` arguments after the scene: the layer index, then
/// `none`, `chroma <r> <g> <b> <similarity> <smoothness> <spill>`,
/// `luma <low> <high> <softness> on|off`, or `fill <key-input> luma|alpha
/// <low> <high> <softness> on|off`, then the options of a refinement that
/// is not the identity (`--clip`, `--choke`, `--edge-softness`,
/// `--despill`).
fn key_arguments(index: usize, key: Option<WireLayerKey>) -> Vec<String> {
    let mut arguments = vec![index.to_string()];
    match key {
        None => arguments.push("none".to_owned()),
        Some(WireLayerKey::Chroma(chroma)) => arguments.extend([
            "chroma".to_owned(),
            chroma.red.to_string(),
            chroma.green.to_string(),
            chroma.blue.to_string(),
            chroma.similarity.to_string(),
            chroma.smoothness.to_string(),
            chroma.spill.to_string(),
        ]),
        Some(WireLayerKey::Luma(luma)) => arguments.extend([
            "luma".to_owned(),
            luma.low.to_string(),
            luma.high.to_string(),
            luma.softness.to_string(),
            if luma.invert { "on" } else { "off" }.to_owned(),
        ]),
        Some(WireLayerKey::Fill(fill)) => arguments.extend([
            "fill".to_owned(),
            fill.key_input.to_string(),
            if fill.alpha_channel { "alpha" } else { "luma" }.to_owned(),
            fill.low.to_string(),
            fill.high.to_string(),
            fill.softness.to_string(),
            if fill.invert { "on" } else { "off" }.to_owned(),
        ]),
    }
    if let Some(key) = key {
        let refinement = key.refinement();
        let identity = fm_protocol::WireKeyRefinement::NONE;
        if (refinement.clip_black, refinement.clip_white)
            != (identity.clip_black, identity.clip_white)
        {
            arguments.extend([
                "--clip".to_owned(),
                refinement.clip_black.to_string(),
                refinement.clip_white.to_string(),
            ]);
        }
        if refinement.choke != 0 {
            arguments.extend(["--choke".to_owned(), refinement.choke.to_string()]);
        }
        if refinement.edge_softness != 0 {
            arguments.extend([
                "--edge-softness".to_owned(),
                refinement.edge_softness.to_string(),
            ]);
        }
        match refinement.despill {
            fm_protocol::WireKeyDespill::Off => {}
            fm_protocol::WireKeyDespill::Green { strength } => arguments.extend([
                "--despill".to_owned(),
                "green".to_owned(),
                strength.to_string(),
            ]),
            fm_protocol::WireKeyDespill::Blue { strength } => arguments.extend([
                "--despill".to_owned(),
                "blue".to_owned(),
                strength.to_string(),
            ]),
        }
        if refinement.light_wrap != 0 {
            arguments.extend([
                "--light-wrap".to_owned(),
                refinement.light_wrap.to_string(),
                refinement.light_wrap_width.to_string(),
            ]);
        }
        if let Some(matte) = refinement.garbage_matte {
            let points = matte
                .points()
                .iter()
                .map(|point| format!("{}:{}", point.x, point.y))
                .collect::<Vec<_>>()
                .join(",");
            arguments.extend([
                "--matte".to_owned(),
                if matte.invert() { "outside" } else { "inside" }.to_owned(),
                points,
            ]);
            if matte.feather() != 0 {
                arguments.extend(["--matte-feather".to_owned(), matte.feather().to_string()]);
            }
        }
    }
    arguments
}

impl SceneLayerEdit {
    /// The `freemix-cli` subcommand and its arguments after the project
    /// path and scene id.
    #[must_use]
    pub fn cli_arguments(&self) -> (&'static str, Vec<String>) {
        match self.clone() {
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
            Self::Key { index, key } => ("scene-layer-key", key_arguments(index, key)),
            Self::ClearEffects { index } => ("scene-layer-effect-clear", vec![index.to_string()]),
            Self::AddEffect { index, effect } => ("scene-layer-effect-add", {
                let mut arguments = vec![index.to_string(), shell_word(&effect.name)];
                arguments.extend(effect.parameters.iter().map(ToString::to_string));
                arguments
            }),
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
