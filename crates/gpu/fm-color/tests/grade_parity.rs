//! Pins `fm-grade`'s transfer functions and LUT sampling to the canonical
//! `fm-color` implementations bit for bit.

use fm_color::{Lut1D, Lut3D, Rgb, srgb_from_linear, srgb_to_linear};
use fm_grade::{CubeLut, srgb_eotf, srgb_oetf};

/// Deterministic xorshift values in `[0, 1)`.
struct Noise(u32);

impl Noise {
    #[allow(clippy::cast_precision_loss)]
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / (1_u32 << 24) as f32
    }
}

#[allow(clippy::cast_precision_loss)]
fn sweep() -> impl Iterator<Item = f32> {
    (0..=255_u8)
        .map(|code| f32::from(code) / 255.0)
        .chain((0..=8192_u16).map(|index| f32::from(index) / 8192.0))
}

#[test]
fn srgb_eotf_matches_fm_color_bit_exactly() {
    for value in sweep() {
        assert_eq!(
            srgb_eotf(value).to_bits(),
            srgb_to_linear(value).to_bits(),
            "{value}"
        );
    }
}

#[test]
fn srgb_oetf_matches_fm_color_for_non_negative_light() {
    for value in sweep().chain([1.5, 2.0, 4.0]) {
        assert_eq!(
            srgb_oetf(value).to_bits(),
            srgb_from_linear(value).to_bits(),
            "{value}"
        );
    }
    // fm-grade clips negative light to zero; fm-color extends the linear toe.
    assert_eq!(srgb_oetf(-0.5), 0.0);
    // Light above 1 extends the power law in both.
    assert!(srgb_oetf(2.0) > 1.0);
}

#[test]
fn cube_3d_sampling_matches_lut3d_bit_exactly() {
    let mut noise = Noise(0x9E37_79B9);
    let edge = 7;
    let entries: Vec<[f32; 3]> = (0..edge * edge * edge)
        .map(|_| [noise.next(), noise.next() * 1.5, noise.next() - 0.25])
        .collect();
    let minimum = [-0.1, 0.0, 0.05];
    let maximum = [1.1, 1.0, 0.9];
    let cube = CubeLut::new_3d(edge, entries.clone(), minimum, maximum).unwrap();
    let reference = Lut3D::new(
        edge,
        entries
            .iter()
            .map(|[r, g, b]| Rgb::new(*r, *g, *b))
            .collect(),
        Rgb::new(minimum[0], minimum[1], minimum[2]),
        Rgb::new(maximum[0], maximum[1], maximum[2]),
    )
    .unwrap();
    for _ in 0..20_000 {
        let input = [
            noise.next() * 1.4 - 0.2,
            noise.next() * 1.4 - 0.2,
            noise.next() * 1.4 - 0.2,
        ];
        let expected = reference.sample(Rgb::new(input[0], input[1], input[2]));
        let actual = cube.sample(input);
        assert_eq!(
            actual.map(f32::to_bits),
            [expected.r, expected.g, expected.b].map(f32::to_bits),
            "{input:?}"
        );
    }
}

#[test]
fn cube_1d_sampling_matches_lut1d_bit_exactly() {
    let mut noise = Noise(0x1234_5678);
    let entries: Vec<[f32; 3]> = (0..33)
        .map(|_| [noise.next(), noise.next(), noise.next()])
        .collect();
    let cube = CubeLut::new_1d(entries.clone(), [0.0; 3], [1.0; 3]).unwrap();
    let reference = Lut1D::new(
        entries
            .iter()
            .map(|[r, g, b]| Rgb::new(*r, *g, *b))
            .collect(),
        Rgb::new(0.0, 0.0, 0.0),
        Rgb::new(1.0, 1.0, 1.0),
    )
    .unwrap();
    for _ in 0..20_000 {
        let input = [noise.next(), noise.next() * 1.2 - 0.1, noise.next()];
        let expected = reference.sample(Rgb::new(input[0], input[1], input[2]));
        assert_eq!(
            cube.sample(input).map(f32::to_bits),
            [expected.r, expected.g, expected.b].map(f32::to_bits),
            "{input:?}"
        );
    }
}
