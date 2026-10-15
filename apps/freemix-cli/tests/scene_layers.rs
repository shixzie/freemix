//! IN-020 acceptance: one background plus ten positioned foreground layers,
//! each with its own crop, transform, key and effect, authored through the
//! local CLI, rendered through the compositor, and checked pixel for pixel
//! against an analytic reference.
//!
//! Every foreground layer crops an 8x6 window straddling the boundary of two
//! 75 % colour bars (4 px of each), scales it 2x into a 16x12 cell at its own
//! position, keys one of the two bars away (chroma or luma, sometimes
//! inverted), and runs one reference effect. The reference is computed here
//! from the documented key and effect formulas, so the render must match the
//! cell halves exactly and the solid background everywhere outside the
//! cells.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

const WIDTH: u32 = 200;
const HEIGHT: u32 = 32;
/// The colour `freemix-cli new` gives input 1, the background layer.
const BACKGROUND: [u8; 3] = [73, 151, 199];
/// 75 % BT.709 bars as full-range 8-bit codes: white, yellow, cyan, green,
/// magenta, red, blue, black. At 200 px each bar is exactly 25 px wide.
const BARS: [[u8; 3]; 8] = [
    [191, 191, 191],
    [191, 191, 0],
    [0, 191, 191],
    [0, 191, 0],
    [191, 0, 191],
    [191, 0, 0],
    [0, 0, 191],
    [0, 0, 0],
];
const BAR_WIDTH: u32 = WIDTH / 8;
const CELL_WIDTH: u32 = 16;
const CELL_HEIGHT: u32 = 12;
const CELL_Y: u32 = 10;

#[derive(Clone, Copy)]
enum Key {
    Chroma { rgb: [u8; 3], similarity: u8 },
    Luma { low: u8, high: u8, invert: bool },
}

#[derive(Clone, Copy)]
enum Effect {
    Levels(u8, u8),
    BrightnessContrast(i32, i32),
    Saturation(i32),
    Sharpen(i32),
    FlipHorizontal,
}

struct Foreground {
    /// The left bar of the cropped pair; the right one is `bar + 1`.
    bar: usize,
    key: Key,
    effect: Effect,
}

fn cell_x(layer: usize) -> u32 {
    2 + 20 * u32::try_from(layer).unwrap()
}

/// The ten foreground layers: every key kind (chroma, banded luma,
/// inverted luma) and every reference effect, over every bar boundary.
const FOREGROUND: [Foreground; 10] = [
    Foreground {
        bar: 0,
        key: Key::Chroma {
            rgb: [191, 191, 0],
            similarity: 8,
        },
        effect: Effect::Levels(0, 128),
    },
    Foreground {
        bar: 1,
        key: Key::Luma {
            low: 160,
            high: 255,
            invert: false,
        },
        effect: Effect::BrightnessContrast(-50, 0),
    },
    Foreground {
        bar: 2,
        key: Key::Luma {
            low: 140,
            high: 255,
            invert: true,
        },
        effect: Effect::Saturation(-100),
    },
    Foreground {
        bar: 3,
        key: Key::Chroma {
            rgb: [0, 191, 0],
            similarity: 8,
        },
        effect: Effect::FlipHorizontal,
    },
    Foreground {
        bar: 4,
        key: Key::Luma {
            low: 45,
            high: 255,
            invert: false,
        },
        effect: Effect::Sharpen(500),
    },
    Foreground {
        bar: 5,
        key: Key::Luma {
            low: 20,
            high: 100,
            invert: false,
        },
        effect: Effect::BrightnessContrast(0, 50),
    },
    Foreground {
        bar: 6,
        key: Key::Chroma {
            rgb: [0, 0, 0],
            similarity: 4,
        },
        effect: Effect::Levels(64, 192),
    },
    Foreground {
        bar: 0,
        key: Key::Luma {
            low: 180,
            high: 255,
            invert: false,
        },
        effect: Effect::FlipHorizontal,
    },
    Foreground {
        bar: 1,
        key: Key::Chroma {
            rgb: [191, 191, 0],
            similarity: 8,
        },
        effect: Effect::Saturation(100),
    },
    Foreground {
        bar: 2,
        key: Key::Luma {
            low: 0,
            high: 145,
            invert: false,
        },
        effect: Effect::Sharpen(1000),
    },
];

/// The compositor's BT.709 luma of a straight 8-bit colour.
fn luma(rgb: [u8; 3]) -> u32 {
    (u32::from(rgb[0]) * 54 + u32::from(rgb[1]) * 183 + u32::from(rgb[2]) * 19 + 128) / 256
}

/// Whether `key` keeps `rgb` (every key here is hard: softness 0 and colours
/// well clear of every threshold).
fn kept(key: Key, rgb: [u8; 3]) -> bool {
    match key {
        Key::Chroma {
            rgb: target,
            similarity,
        } => {
            let distance = (0..3)
                .map(|channel| rgb[channel].abs_diff(target[channel]))
                .max()
                .unwrap();
            distance > similarity
        }
        Key::Luma { low, high, invert } => {
            let value = luma(rgb);
            let inside = value > u32::from(low) && (high == 255 || value < u32::from(high));
            inside != invert
        }
    }
}

/// Rounds `value / 100` half away from zero.
fn round_div_100(value: i32) -> i32 {
    let rounded = (value.abs() * 2 + 100) / 200;
    rounded * value.signum()
}

fn clamp_u8(value: i32) -> u8 {
    u8::try_from(value.clamp(0, 255)).unwrap()
}

/// `effect` on a flat colour (sharpen and flip leave a flat area's colour
/// unchanged; flip moves the halves instead).
fn effected(effect: Effect, rgb: [u8; 3]) -> [u8; 3] {
    match effect {
        Effect::Levels(black, white) => rgb.map(|channel| {
            let span = i32::from(white) - i32::from(black);
            clamp_u8(((i32::from(channel) - i32::from(black)) * 255 + span / 2) / span)
        }),
        Effect::BrightnessContrast(brightness, contrast) => rgb.map(|channel| {
            clamp_u8(
                round_div_100((i32::from(channel) - 128) * (100 + contrast)) + 128 + brightness,
            )
        }),
        Effect::Saturation(amount) => {
            let luma =
                (77 * i32::from(rgb[0]) + 150 * i32::from(rgb[1]) + 29 * i32::from(rgb[2]) + 128)
                    >> 8;
            rgb.map(|channel| {
                clamp_u8(luma + round_div_100((i32::from(channel) - luma) * (100 + amount)))
            })
        }
        Effect::Sharpen(_) | Effect::FlipHorizontal => rgb,
    }
}

/// The expected colours of a foreground cell's left and right halves.
fn expected_halves(layer: &Foreground) -> [[u8; 3]; 2] {
    let shown = |rgb: [u8; 3]| {
        if kept(layer.key, rgb) {
            effected(layer.effect, rgb)
        } else {
            BACKGROUND
        }
    };
    let left = shown(BARS[layer.bar]);
    let right = shown(BARS[layer.bar + 1]);
    if matches!(layer.effect, Effect::FlipHorizontal) {
        [right, left]
    } else {
        [left, right]
    }
}

#[allow(clippy::too_many_lines)]
#[test]
fn background_and_ten_cropped_transformed_keyed_effected_layers_render_exactly() {
    let root = unique_root();
    fs::create_dir_all(&root).unwrap();
    let project = root.join("layers.freemix");
    let path = project.to_str().unwrap();
    let render = root.join("program.ppm");

    apply(&["new", path]);
    // Scene renders compose at the project size: a 200x32 project keeps the
    // bars 25 px wide and the composition small enough for a debug build.
    let mut source = fs::read_to_string(project.join("project.json")).unwrap();
    replace_once(
        &mut source,
        "\"width\": 1920,",
        &format!("\"width\": {WIDTH},"),
    );
    replace_once(
        &mut source,
        "\"height\": 1080,",
        &format!("\"height\": {HEIGHT},"),
    );
    fs::write(project.join("project.json"), source).unwrap();
    apply(&["input-add", path, "4", "Bars"]);
    apply(&["scene-input-add", path, "3", "11", "Program"]);
    // The background: input 1's solid colour over the whole canvas.
    apply(&["scene-layer-add", path, "11", "1", "0", "Background"]);
    for (index, layer) in FOREGROUND.iter().enumerate() {
        let scene_index = (index + 1).to_string();
        let index_arg = scene_index.as_str();
        let z = scene_index.clone();
        apply(&[
            "scene-layer-add",
            path,
            "11",
            "4",
            &z,
            &format!("Layer {}", index + 1),
        ]);
        // Crop: 4 px of bar `bar` and 4 px of the next one.
        let crop_x = BAR_WIDTH * (u32::try_from(layer.bar).unwrap() + 1) - 4;
        apply(&[
            "scene-layer-crop",
            path,
            "11",
            index_arg,
            &crop_x.to_string(),
            "4",
            "8",
            "6",
        ]);
        // Transform: a 2x scale into this layer's own cell.
        apply(&[
            "scene-layer-geometry",
            path,
            "11",
            index_arg,
            &cell_x(index).to_string(),
            &CELL_Y.to_string(),
            &CELL_WIDTH.to_string(),
            &CELL_HEIGHT.to_string(),
            "0",
        ]);
        let key: Vec<String> = match layer.key {
            Key::Chroma { rgb, similarity } => vec![
                "chroma".into(),
                rgb[0].to_string(),
                rgb[1].to_string(),
                rgb[2].to_string(),
                similarity.to_string(),
                "0".into(),
                "0".into(),
            ],
            Key::Luma { low, high, invert } => vec![
                "luma".into(),
                low.to_string(),
                high.to_string(),
                "0".into(),
                if invert { "on" } else { "off" }.into(),
            ],
        };
        let mut arguments = vec!["scene-layer-key", path, "11", index_arg];
        arguments.extend(key.iter().map(String::as_str));
        apply(&arguments);
        let effect: Vec<String> = match layer.effect {
            Effect::Levels(black, white) => {
                vec!["levels".into(), black.to_string(), white.to_string()]
            }
            Effect::BrightnessContrast(brightness, contrast) => vec![
                "brightness_contrast".into(),
                brightness.to_string(),
                contrast.to_string(),
            ],
            Effect::Saturation(amount) => vec!["saturation".into(), amount.to_string()],
            Effect::Sharpen(amount) => vec!["sharpen".into(), amount.to_string()],
            Effect::FlipHorizontal => vec!["flip".into(), "1".into(), "0".into()],
        };
        let mut arguments = vec!["scene-layer-effect-add", path, "11", index_arg];
        arguments.extend(effect.iter().map(String::as_str));
        apply(&arguments);
    }

    // The saved scene lists all eleven layers, each foreground one keyed.
    let scenes = invoke(&["scenes", path]);
    assert_success(&scenes);
    let listing = String::from_utf8(scenes.stdout).unwrap();
    assert_eq!(listing.matches("  layer index=").count(), 11, "{listing}");
    assert_eq!(listing.matches("key=none").count(), 1, "{listing}");
    assert_eq!(listing.matches("effects=none").count(), 1, "{listing}");

    apply(&["preview", path, "3"]);
    apply(&["cut", path]);
    apply(&[
        "render",
        path,
        render.to_str().unwrap(),
        "--width",
        &WIDTH.to_string(),
        "--height",
        &HEIGHT.to_string(),
    ]);
    let image = Ppm::read(&render, WIDTH, HEIGHT);

    // Outside every cell the background layer shows unchanged.
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let in_cell = (CELL_Y..CELL_Y + CELL_HEIGHT).contains(&y)
                && (0..FOREGROUND.len())
                    .any(|layer| (cell_x(layer)..cell_x(layer) + CELL_WIDTH).contains(&x));
            if !in_cell {
                assert_eq!(image.pixel(x, y), BACKGROUND, "background at ({x}, {y})");
            }
        }
    }
    // Inside each cell: the left 6 and right 6 columns are the keyed,
    // effected bars (the middle 4 columns are the 2x-scaled source columns
    // next to the bar boundary, which sharpen deliberately changes).
    for (index, layer) in FOREGROUND.iter().enumerate() {
        let [left, right] = expected_halves(layer);
        for y in CELL_Y..CELL_Y + CELL_HEIGHT {
            for column in (0..6).chain(10..CELL_WIDTH) {
                let x = cell_x(index) + column;
                let expected = if column < 6 { left } else { right };
                assert_eq!(
                    image.pixel(x, y),
                    expected,
                    "layer {} ({x}, {y})",
                    index + 1
                );
            }
        }
        // Every layer keys exactly one of its two bars away.
        assert!(
            (left == BACKGROUND) != (right == BACKGROUND),
            "layer {} halves {left:?} {right:?}",
            index + 1
        );
    }

    // Clearing every key and effect leaves the plain cropped bars.
    for index in 1..=FOREGROUND.len() {
        let index = index.to_string();
        apply(&["scene-layer-key", path, "11", &index, "none"]);
        apply(&["scene-layer-effect-clear", path, "11", &index]);
    }
    apply(&[
        "render",
        path,
        render.to_str().unwrap(),
        "--width",
        &WIDTH.to_string(),
        "--height",
        &HEIGHT.to_string(),
    ]);
    let plain = Ppm::read(&render, WIDTH, HEIGHT);
    for (index, layer) in FOREGROUND.iter().enumerate() {
        for y in CELL_Y..CELL_Y + CELL_HEIGHT {
            for column in 0..CELL_WIDTH {
                let x = cell_x(index) + column;
                let bar = layer.bar + usize::from(column >= CELL_WIDTH / 2);
                assert_eq!(
                    plain.pixel(x, y),
                    BARS[bar],
                    "plain layer {} ({x}, {y})",
                    index + 1
                );
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}

struct Ppm {
    width: u32,
    pixels: Vec<u8>,
}

impl Ppm {
    fn read(path: &Path, width: u32, height: u32) -> Self {
        let bytes = fs::read(path).unwrap();
        let header = format!("P6\n{width} {height}\n255\n");
        assert!(bytes.starts_with(header.as_bytes()));
        let pixels = bytes[header.len()..].to_vec();
        assert_eq!(pixels.len(), (width * height * 3) as usize);
        Self { width, pixels }
    }

    fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let offset = ((y * self.width + x) * 3) as usize;
        [
            self.pixels[offset],
            self.pixels[offset + 1],
            self.pixels[offset + 2],
        ]
    }
}

fn apply(arguments: &[&str]) {
    assert_success(&invoke(arguments));
}

fn invoke(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_freemix-cli"))
        .args(arguments)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "process failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn replace_once(source: &mut String, from: &str, to: &str) {
    assert!(source.contains(from), "manifest did not contain `{from}`");
    *source = source.replacen(from, to, 1);
}

fn unique_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "freemix-cli-layers-{}-{nonce}-{sequence}",
        std::process::id()
    ))
}
