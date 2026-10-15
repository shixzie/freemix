//! A small CPU renderer for widget screenshots in tests: it rasterizes the
//! meshes egui tessellates (with the textures egui uploaded) into an RGBA8
//! image and encodes it as PNG, so documentation captures need no window,
//! GPU, or extra dependency.
//!
//! Blending follows egui's reference painters: premultiplied sRGB-encoded
//! colours blended in gamma space, with egui's own feathering providing the
//! anti-aliasing. Triangles follow a top-left fill rule so shared edges are
//! drawn once.

use std::collections::HashMap;

use egui::epaint::{ClippedPrimitive, ImageData, Primitive, TextureId, Vertex};
use egui::{Color32, ColorImage, FullOutput, Rect};

/// Textures as egui uploaded them, accumulated over passes.
#[derive(Default)]
pub(crate) struct Textures {
    images: HashMap<TextureId, ColorImage>,
}

impl Textures {
    /// Applies one pass's texture changes.
    pub(crate) fn apply(&mut self, output: &FullOutput) {
        for (id, delta) in &output.textures_delta.set {
            let ImageData::Color(image) = &delta.image;
            match delta.pos {
                None => {
                    self.images.insert(*id, (**image).clone());
                }
                Some([x0, y0]) => {
                    if let Some(target) = self.images.get_mut(id) {
                        for y in 0..image.size[1] {
                            for x in 0..image.size[0] {
                                let index = (y0 + y) * target.size[0] + x0 + x;
                                if let Some(pixel) = target.pixels.get_mut(index) {
                                    *pixel = image.pixels[y * image.size[0] + x];
                                }
                            }
                        }
                    }
                }
            }
        }
        for id in &output.textures_delta.free {
            self.images.remove(id);
        }
    }

    /// Bilinear, clamped, premultiplied sample at normalized `uv`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    fn sample(&self, id: TextureId, uv: [f32; 2]) -> [f32; 4] {
        let Some(image) = self.images.get(&id) else {
            return [1.0; 4];
        };
        let (width, height) = (image.size[0], image.size[1]);
        if width == 0 || height == 0 {
            return [1.0; 4];
        }
        let x = (uv[0] * width as f32 - 0.5).clamp(0.0, (width - 1) as f32);
        let y = (uv[1] * height as f32 - 0.5).clamp(0.0, (height - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let texel = |x: usize, y: usize| unit(image.pixels[y * width + x]);
        let mix = |a: [f32; 4], b: [f32; 4], t: f32| -> [f32; 4] {
            core::array::from_fn(|channel| a[channel] + (b[channel] - a[channel]) * t)
        };
        mix(
            mix(texel(x0, y0), texel(x1, y0), fx),
            mix(texel(x0, y1), texel(x1, y1), fx),
            fy,
        )
    }
}

fn unit(color: Color32) -> [f32; 4] {
    color.to_array().map(|channel| f32::from(channel) / 255.0)
}

/// A premultiplied RGBA canvas.
pub(crate) struct Canvas {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pixels: Vec<[f32; 4]>,
}

impl Canvas {
    pub(crate) fn new(width: usize, height: usize, background: Color32) -> Self {
        Self {
            width,
            height,
            pixels: vec![unit(background); width * height],
        }
    }

    /// Paints tessellated primitives at `pixels_per_point`.
    pub(crate) fn paint(
        &mut self,
        primitives: &[ClippedPrimitive],
        textures: &Textures,
        pixels_per_point: f32,
    ) {
        for primitive in primitives {
            let Primitive::Mesh(mesh) = &primitive.primitive else {
                continue;
            };
            let clip = Rect::from_min_max(
                (primitive.clip_rect.min.to_vec2() * pixels_per_point).to_pos2(),
                (primitive.clip_rect.max.to_vec2() * pixels_per_point).to_pos2(),
            );
            for triangle in mesh.indices.as_chunks::<3>().0 {
                let vertices = [
                    mesh.vertices[triangle[0] as usize],
                    mesh.vertices[triangle[1] as usize],
                    mesh.vertices[triangle[2] as usize],
                ];
                self.triangle(vertices, mesh.texture_id, textures, clip, pixels_per_point);
            }
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        clippy::many_single_char_names
    )]
    fn triangle(
        &mut self,
        vertices: [Vertex; 3],
        texture: TextureId,
        textures: &Textures,
        clip: Rect,
        scale: f32,
    ) {
        let p = vertices.map(|vertex| [vertex.pos.x * scale, vertex.pos.y * scale]);
        let edge = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
            (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
        };
        let area = edge(p[0], p[1], p[2]);
        if area.abs() < 1e-6 {
            return;
        }
        // Wind counter-clockwise in screen space (y down) for the fill rule.
        let (order, area) = if area < 0.0 {
            ([0, 2, 1], -area)
        } else {
            ([0, 1, 2], area)
        };
        let p = order.map(|index| p[index]);
        let vertices = order.map(|index| vertices[index]);
        let min_x = p
            .iter()
            .map(|q| q[0])
            .fold(f32::MAX, f32::min)
            .max(clip.min.x)
            .max(0.0);
        let max_x = p
            .iter()
            .map(|q| q[0])
            .fold(f32::MIN, f32::max)
            .min(clip.max.x)
            .min(self.width as f32);
        let min_y = p
            .iter()
            .map(|q| q[1])
            .fold(f32::MAX, f32::min)
            .max(clip.min.y)
            .max(0.0);
        let max_y = p
            .iter()
            .map(|q| q[1])
            .fold(f32::MIN, f32::max)
            .min(clip.max.y)
            .min(self.height as f32);
        if min_x >= max_x || min_y >= max_y {
            return;
        }
        // An edge owns its pixels when it is a top or a left edge.
        let owns = |a: [f32; 2], b: [f32; 2]| {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            (dy == 0.0 && dx > 0.0) || dy < 0.0
        };
        let edges = [(p[1], p[2]), (p[2], p[0]), (p[0], p[1])];
        let owned = edges.map(|(a, b)| owns(a, b));
        let colors = vertices.map(|vertex| unit(vertex.color));
        let uvs = vertices.map(|vertex| [vertex.uv.x, vertex.uv.y]);
        for y in (min_y.floor() as usize)..(max_y.ceil() as usize) {
            for x in (min_x.floor() as usize)..(max_x.ceil() as usize) {
                let c = [x as f32 + 0.5, y as f32 + 0.5];
                if c[0] < clip.min.x
                    || c[0] >= clip.max.x
                    || c[1] < clip.min.y
                    || c[1] >= clip.max.y
                {
                    continue;
                }
                let w = edges.map(|(a, b)| edge(a, b, c));
                if (0..3).any(|i| w[i] < 0.0 || (w[i] == 0.0 && !owned[i])) {
                    continue;
                }
                let w = w.map(|weight| weight / area);
                let color: [f32; 4] = core::array::from_fn(|channel| {
                    w[0] * colors[0][channel]
                        + w[1] * colors[1][channel]
                        + w[2] * colors[2][channel]
                });
                let uv = [
                    w[0] * uvs[0][0] + w[1] * uvs[1][0] + w[2] * uvs[2][0],
                    w[0] * uvs[0][1] + w[1] * uvs[1][1] + w[2] * uvs[2][1],
                ];
                let texel = textures.sample(texture, uv);
                let source: [f32; 4] =
                    core::array::from_fn(|channel| texel[channel] * color[channel]);
                let target = &mut self.pixels[y * self.width + x];
                let keep = 1.0 - source[3];
                for channel in 0..4 {
                    target[channel] = source[channel] + target[channel] * keep;
                }
            }
        }
    }

    /// The `rect` (in pixels) of the canvas as straight RGBA8.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    pub(crate) fn crop_rgba8(&self, rect: Rect) -> (usize, usize, Vec<u8>) {
        let x0 = (rect.min.x.max(0.0) as usize).min(self.width);
        let y0 = (rect.min.y.max(0.0) as usize).min(self.height);
        let x1 = (rect.max.x.ceil().max(0.0) as usize)
            .min(self.width)
            .max(x0);
        let y1 = (rect.max.y.ceil().max(0.0) as usize)
            .min(self.height)
            .max(y0);
        let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
        for y in y0..y1 {
            for x in x0..x1 {
                let [r, g, b, a] = self.pixels[y * self.width + x];
                let straight = |value: f32| {
                    let value = if a > 0.0 { value / a } else { 0.0 };
                    (value.clamp(0.0, 1.0) * 255.0).round() as u8
                };
                out.extend_from_slice(&[
                    straight(r),
                    straight(g),
                    straight(b),
                    (a.clamp(0.0, 1.0) * 255.0).round() as u8,
                ]);
            }
        }
        (x1 - x0, y1 - y0, out)
    }
}

/// Encodes straight RGBA8 as a PNG with per-row adaptive filtering and a
/// fixed-Huffman LZ77 deflate stream.
pub(crate) fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Vec<u8> {
    let stride = width * 4;
    let mut raw = Vec::with_capacity((stride + 1) * height);
    let zero = vec![0_u8; stride];
    for y in 0..height {
        let row = &rgba[y * stride..(y + 1) * stride];
        let above = if y == 0 {
            &zero[..]
        } else {
            &rgba[(y - 1) * stride..y * stride]
        };
        let filtered: Vec<(u8, Vec<u8>)> = (0..=4_u8)
            .map(|kind| (kind, filter_row(kind, row, above)))
            .collect();
        let (kind, best) = filtered
            .into_iter()
            .min_by_key(|(_, bytes)| {
                bytes
                    .iter()
                    .map(|&byte| u64::from(byte.cast_signed().unsigned_abs()))
                    .sum::<u64>()
            })
            .unwrap_or_default();
        raw.push(kind);
        raw.extend_from_slice(&best);
    }
    let mut zlib = vec![0x78, 0x01];
    zlib.extend_from_slice(&deflate_fixed(&raw));
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&u32::try_from(width).unwrap_or(0).to_be_bytes());
    header.extend_from_slice(&u32::try_from(height).unwrap_or(0).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut png, *b"IHDR", &header);
    chunk(&mut png, *b"IDAT", &zlib);
    chunk(&mut png, *b"IEND", &[]);
    png
}

fn filter_row(kind: u8, row: &[u8], above: &[u8]) -> Vec<u8> {
    (0..row.len())
        .map(|index| {
            let left = if index >= 4 { row[index - 4] } else { 0 };
            let up = above[index];
            let corner = if index >= 4 { above[index - 4] } else { 0 };
            let predictor = match kind {
                0 => 0,
                1 => left,
                2 => up,
                3 => u8::try_from(u16::midpoint(u16::from(left), u16::from(up))).unwrap_or(0),
                _ => paeth(left, up, corner),
            };
            row[index].wrapping_sub(predictor)
        })
        .collect()
}

fn paeth(left: u8, up: u8, corner: u8) -> u8 {
    let estimate = i16::from(left) + i16::from(up) - i16::from(corner);
    let distance = |value: u8| (estimate - i16::from(value)).abs();
    if distance(left) <= distance(up) && distance(left) <= distance(corner) {
        left
    } else if distance(up) <= distance(corner) {
        up
    } else {
        corner
    }
}

fn chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1_u32, 0_u32);
    for &byte in bytes {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// LSB-first bit writer for DEFLATE.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    buffer: u64,
    count: u32,
}

impl Bits {
    fn put(&mut self, value: u32, bits: u32) {
        self.buffer |= u64::from(value) << self.count;
        self.count += bits;
        while self.count >= 8 {
            self.out.push((self.buffer & 0xFF) as u8);
            self.buffer >>= 8;
            self.count -= 8;
        }
    }

    /// A Huffman code, which DEFLATE stores most significant bit first.
    fn code(&mut self, code: u32, bits: u32) {
        self.put(code.reverse_bits() >> (32 - bits), bits);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.out.push((self.buffer & 0xFF) as u8);
        }
        self.out
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn literal(bits: &mut Bits, symbol: u32) {
    match symbol {
        0..=143 => bits.code(0x30 + symbol, 8),
        144..=255 => bits.code(0x190 + symbol - 144, 9),
        256..=279 => bits.code(symbol - 256, 7),
        _ => bits.code(0xC0 + symbol - 280, 8),
    }
}

/// One fixed-Huffman block with greedy hash-chain LZ77 matching.
fn deflate_fixed(data: &[u8]) -> Vec<u8> {
    const WINDOW: usize = 32_768;
    const CHAIN: usize = 24;
    let mut bits = Bits::default();
    bits.put(1, 1); // final block
    bits.put(1, 2); // fixed Huffman
    let mut head: HashMap<[u8; 3], usize> = HashMap::new();
    let mut previous = vec![usize::MAX; data.len()];
    let mut index = 0;
    let insert = |head: &mut HashMap<[u8; 3], usize>, previous: &mut [usize], position: usize| {
        if position + 3 <= data.len() {
            let key = [data[position], data[position + 1], data[position + 2]];
            if let Some(last) = head.insert(key, position) {
                previous[position] = last;
            }
        }
    };
    while index < data.len() {
        let mut best = (0_usize, 0_usize);
        if index + 3 <= data.len() {
            let key = [data[index], data[index + 1], data[index + 2]];
            let mut candidate = head.get(&key).copied();
            let mut steps = 0;
            while let Some(start) = candidate {
                if index - start > WINDOW || steps == CHAIN {
                    break;
                }
                let limit = (data.len() - index).min(258);
                let length = (0..limit)
                    .take_while(|&offset| data[start + offset] == data[index + offset])
                    .count();
                if length > best.0 {
                    best = (length, index - start);
                    if length == limit {
                        break;
                    }
                }
                candidate = (previous[start] != usize::MAX).then(|| previous[start]);
                steps += 1;
            }
        }
        let (length, distance) = best;
        if length >= 3 {
            let code = LENGTH_BASE.partition_point(|&base| usize::from(base) <= length) - 1;
            literal(&mut bits, 257 + u32::try_from(code).unwrap_or(0));
            bits.put(
                u32::try_from(length).unwrap_or(0) - u32::from(LENGTH_BASE[code]),
                u32::from(LENGTH_EXTRA[code]),
            );
            let code = DIST_BASE.partition_point(|&base| usize::from(base) <= distance) - 1;
            bits.code(u32::try_from(code).unwrap_or(0), 5);
            bits.put(
                u32::try_from(distance).unwrap_or(0) - u32::from(DIST_BASE[code]),
                u32::from(DIST_EXTRA[code]),
            );
            for position in index..index + length {
                insert(&mut head, &mut previous, position);
            }
            index += length;
        } else {
            literal(&mut bits, u32::from(data[index]));
            insert(&mut head, &mut previous, index);
            index += 1;
        }
    }
    literal(&mut bits, 256);
    bits.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inflates a fixed-Huffman stream back, to check the encoder.
    fn inflate_fixed(stream: &[u8]) -> Vec<u8> {
        struct Reader<'a> {
            bytes: &'a [u8],
            position: usize,
        }
        impl Reader<'_> {
            fn bit(&mut self) -> u32 {
                let bit = (self.bytes[self.position / 8] >> (self.position % 8)) & 1;
                self.position += 1;
                u32::from(bit)
            }
            fn bits(&mut self, count: u32) -> u32 {
                (0..count).fold(0, |value, shift| value | (self.bit() << shift))
            }
            fn code(&mut self, count: u32) -> u32 {
                (0..count).fold(0, |value, _| (value << 1) | self.bit())
            }
        }
        let mut reader = Reader {
            bytes: stream,
            position: 3,
        };
        let mut out = Vec::new();
        loop {
            let mut code = reader.code(7);
            let symbol = if code <= 0x17 {
                code + 256
            } else {
                code = (code << 1) | reader.bit();
                if (0x30..=0xBF).contains(&code) {
                    code - 0x30
                } else if (0xC0..=0xC7).contains(&code) {
                    code - 0xC0 + 280
                } else {
                    ((code << 1) | reader.bit()) - 0x190 + 144
                }
            };
            match symbol {
                0..=255 => out.push(u8::try_from(symbol).unwrap()),
                256 => break,
                _ => {
                    let index = (symbol - 257) as usize;
                    let length = usize::from(LENGTH_BASE[index])
                        + reader.bits(u32::from(LENGTH_EXTRA[index])) as usize;
                    let code = reader.code(5) as usize;
                    let distance = usize::from(DIST_BASE[code])
                        + reader.bits(u32::from(DIST_EXTRA[code])) as usize;
                    for _ in 0..length {
                        out.push(out[out.len() - distance]);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn deflate_round_trips() {
        let mut data = b"abcabcabcabcxyzxyz".repeat(40);
        data.extend((0..2_000_u32).map(|value| u8::try_from(value * 7 % 251).unwrap()));
        data.extend(vec![0; 1_000]);
        let stream = deflate_fixed(&data);
        assert!(stream.len() < data.len() / 2, "{} bytes", stream.len());
        assert_eq!(inflate_fixed(&stream), data);
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn rasterizes_a_widget() {
        let context = egui::Context::default();
        let mut textures = Textures::default();
        let mut output = None;
        for _ in 0..2 {
            context.begin_pass(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(120.0, 60.0),
                )),
                ..egui::RawInput::default()
            });
            let ui = egui::Ui::new(
                context.clone(),
                egui::Id::new("offscreen"),
                egui::UiBuilder::new().max_rect(context.viewport_rect()),
            );
            ui.painter().rect_filled(
                Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(40.0, 20.0)),
                0.0,
                Color32::from_rgb(200, 30, 40),
            );
            ui.painter().text(
                egui::pos2(60.0, 20.0),
                egui::Align2::LEFT_CENTER,
                "Hi",
                egui::FontId::proportional(14.0),
                Color32::WHITE,
            );
            drop(ui);
            let full = context.end_pass();
            textures.apply(&full);
            output = Some(full);
        }
        let output = output.unwrap();
        let primitives = context.tessellate(output.shapes, output.pixels_per_point);
        let mut canvas = Canvas::new(120, 60, Color32::BLACK);
        canvas.paint(&primitives, &textures, output.pixels_per_point);
        let (width, height, rgba) = canvas.crop_rgba8(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(120.0, 60.0),
        ));
        assert_eq!((width, height), (120, 60));
        let pixel = |x: usize, y: usize| &rgba[(y * width + x) * 4..(y * width + x) * 4 + 4];
        assert_eq!(pixel(30, 20), [200, 30, 40, 255]);
        assert_eq!(pixel(5, 5), [0, 0, 0, 255]);
        // Text left some light pixels.
        let lit = (60..90)
            .flat_map(|x| (10..30).map(move |y| (x, y)))
            .filter(|&(x, y)| pixel(x, y)[0] > 128)
            .count();
        assert!(lit > 10, "{lit}");
        let png = encode_png(width, height, &rgba);
        assert!(png.starts_with(b"\x89PNG"));
    }
}
