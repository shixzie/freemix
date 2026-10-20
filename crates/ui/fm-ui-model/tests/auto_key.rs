//! SW-008 auto key through the public `fm_ui_model::auto_key` API, on
//! synthetic green and blue screens: flat, noisy, unevenly lit, with a
//! subject in front, with spill on the subject, and big enough to be
//! subsampled.

use fm_protocol::{WireKeyDespill, WireKeyRefinement, WireLayerKey, WireLumaKey};
use fm_ui_model::auto_key::{
    AutoKey, AutoKeyBacking, AutoKeyError, AutoKeyPicture, AutoKeySample, SIMILARITY_MARGIN,
    auto_key,
};

const SKIN: [u8; 3] = [214, 160, 128];
const SHIRT: [u8; 3] = [40, 44, 52];

/// A deterministic picture builder.
struct Frame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Frame {
    fn new(width: u32, height: u32, background: impl Fn(u32, u32) -> [u8; 3]) -> Self {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = background(x, y);
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        Self {
            width,
            height,
            rgba,
        }
    }

    fn fill(&mut self, x0: u32, y0: u32, x1: u32, y1: u32, colour: impl Fn(u32, u32) -> [u8; 3]) {
        for y in y0..y1 {
            for x in x0..x1 {
                let index = ((y * self.width + x) * 4) as usize;
                self.rgba[index..index + 3].copy_from_slice(&colour(x, y));
            }
        }
    }

    /// A presenter: a shirt from the bottom edge up and a head above it.
    fn with_subject(mut self) -> Self {
        let (w, h) = (self.width, self.height);
        self.fill(w * 3 / 8, h / 2, w * 5 / 8, h, |_, _| SHIRT);
        self.fill(w * 7 / 16, h / 4, w * 9 / 16, h / 2, |_, _| SKIN);
        self
    }

    fn picture(&self) -> AutoKeyPicture<'_> {
        AutoKeyPicture::new(self.width, self.height, &self.rgba).unwrap()
    }

    fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let index = ((y * self.width + x) * 4) as usize;
        [self.rgba[index], self.rgba[index + 1], self.rgba[index + 2]]
    }
}

/// The chroma key's distance: the largest per-channel difference.
fn distance(a: [u8; 3], b: [u8; 3]) -> u8 {
    a[0].abs_diff(b[0])
        .max(a[1].abs_diff(b[1]))
        .max(a[2].abs_diff(b[2]))
}

/// A fixed linear congruential generator: noise in `-amplitude..=amplitude`.
fn noise(x: u32, y: u32, channel: u32, amplitude: i32) -> i32 {
    let mut state =
        x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663) ^ channel.wrapping_mul(83_492_791);
    state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
    let span = u32::try_from(2 * amplitude + 1).unwrap();
    i32::try_from((state >> 8) % span).unwrap() - amplitude
}

fn add(colour: [u8; 3], offsets: [i32; 3]) -> [u8; 3] {
    [0, 1, 2].map(|channel| {
        u8::try_from((i32::from(colour[channel]) + offsets[channel]).clamp(0, 255)).unwrap()
    })
}

fn noisy(colour: [u8; 3], x: u32, y: u32, amplitude: i32) -> [u8; 3] {
    add(
        colour,
        [0, 1, 2].map(|channel| noise(x, y, channel, amplitude)),
    )
}

/// The keyed fraction of `pixels` (those within the similarity).
fn keyed(key: &AutoKey, pixels: impl Iterator<Item = [u8; 3]>) -> (usize, usize) {
    let colour = [key.red, key.green, key.blue];
    let mut count = 0;
    let mut total = 0;
    for pixel in pixels {
        total += 1;
        count += usize::from(distance(pixel, colour) <= key.similarity);
    }
    (count, total)
}

fn subject_pixels(frame: &Frame) -> Vec<[u8; 3]> {
    let (w, h) = (frame.width, frame.height);
    let mut pixels = Vec::new();
    for y in h / 2..h {
        for x in w * 3 / 8..w * 5 / 8 {
            pixels.push(frame.pixel(x, y));
        }
    }
    for y in h / 4..h / 2 {
        for x in w * 7 / 16..w * 9 / 16 {
            pixels.push(frame.pixel(x, y));
        }
    }
    pixels
}

fn subject_kept(frame: &Frame, key: &AutoKey) {
    let colour = [key.red, key.green, key.blue];
    let opaque_from = u16::from(key.similarity) + u16::from(key.smoothness);
    for pixel in subject_pixels(frame) {
        assert!(
            u16::from(distance(pixel, colour)) > opaque_from,
            "subject pixel {pixel:?} is not opaque under {key:?}"
        );
    }
}

#[test]
fn flat_green_screen_with_a_subject_keys_its_exact_green() {
    let frame = Frame::new(160, 90, |_, _| [30, 190, 60]).with_subject();
    let key = auto_key(frame.picture(), AutoKeySample::Dominant).unwrap();
    assert_eq!(key.backing, AutoKeyBacking::Green);
    assert_eq!([key.red, key.green, key.blue], [30, 190, 60]);
    assert_eq!(key.similarity, SIMILARITY_MARGIN);
    assert_eq!(key.smoothness, 16);
    // No subject pixel is green-tinted: the lightest despill.
    assert_eq!(key.spill, 128);
    assert_eq!(key.despill, WireKeyDespill::Green { strength: 128 });
    assert_eq!(key.samples, 160 * 90);
    // Every pixel but the subject's is in the backing cluster.
    let subject =
        (160 * 5 / 8 - 160 * 3 / 8) * (90 - 45) + (160 * 9 / 16 - 160 * 7 / 16) * (45 - 22);
    assert_eq!(key.agreeing, 160 * 90 - subject);
    subject_kept(&frame, &key);
}

#[test]
fn noisy_green_screen_keys_the_whole_background_and_keeps_the_subject() {
    let frame = Frame::new(160, 90, |x, y| noisy([40, 180, 70], x, y, 12)).with_subject();
    let key = auto_key(frame.picture(), AutoKeySample::Dominant).unwrap();
    assert_eq!(key.backing, AutoKeyBacking::Green);
    for (got, want) in [key.red, key.green, key.blue]
        .into_iter()
        .zip([40, 180, 70])
    {
        assert!(got.abs_diff(want) <= 2, "{key:?}");
    }
    let background = (0..90)
        .flat_map(|y| (0..160).map(move |x| (x, y)))
        .filter(|&(x, _)| !(160 * 3 / 8 - 1..=160 * 5 / 8).contains(&x))
        .map(|(x, y)| frame.pixel(x, y));
    let (count, total) = keyed(&key, background);
    assert!(
        count * 100 >= total * 95,
        "{count} of {total} keyed: {key:?}"
    );
    assert!(key.similarity <= 12 + 2 + SIMILARITY_MARGIN + 2, "{key:?}");
    subject_kept(&frame, &key);
}

#[test]
fn unevenly_lit_green_screen_stays_one_backing() {
    // Brighter at the top left, falling off to the bottom right by 70
    // codes of green, with mild noise.
    let frame = Frame::new(160, 90, |x, y| {
        let fall = i32::try_from((x * 40) / 160 + (y * 30) / 90).unwrap();
        noisy(add([50, 215, 80], [-fall / 3, -fall, -fall / 2]), x, y, 4)
    })
    .with_subject();
    let key = auto_key(frame.picture(), AutoKeySample::Dominant).unwrap();
    assert_eq!(key.backing, AutoKeyBacking::Green);
    let subject = subject_pixels(&frame).len();
    assert_eq!(
        u64::from(key.agreeing),
        160 * 90 - u64::try_from(subject).unwrap(),
        "every background pixel joins the cluster: {key:?}"
    );
    let background = (0..90)
        .flat_map(|y| (0..160).map(move |x| (x, y)))
        .filter(|&(x, y)| !((160 * 3 / 8..160 * 5 / 8).contains(&x) && y >= 22))
        .map(|(x, y)| frame.pixel(x, y));
    let (count, total) = keyed(&key, background);
    assert!(
        count * 100 >= total * 95,
        "{count} of {total} keyed: {key:?}"
    );
    subject_kept(&frame, &key);
}

#[test]
fn a_large_subject_at_the_edges_does_not_hide_the_backing() {
    // The subject fills the whole bottom third and a side band: a border
    // sampler would see mostly subject.
    let mut frame = Frame::new(160, 90, |x, y| noisy([20, 160, 50], x, y, 6));
    frame.fill(0, 60, 160, 90, |x, y| noisy(SHIRT, x, y, 6));
    frame.fill(0, 0, 30, 60, |x, y| noisy(SKIN, x, y, 6));
    let key = auto_key(frame.picture(), AutoKeySample::Dominant).unwrap();
    assert_eq!(key.backing, AutoKeyBacking::Green);
    assert!(
        key.green.abs_diff(160) <= 2 && key.red.abs_diff(20) <= 2,
        "{key:?}"
    );
    assert_eq!(key.agreeing, 130 * 60);
}

#[test]
fn blue_screen_gets_a_blue_despill() {
    let frame = Frame::new(160, 90, |x, y| noisy([20, 50, 200], x, y, 8)).with_subject();
    let key = auto_key(frame.picture(), AutoKeySample::Dominant).unwrap();
    assert_eq!(key.backing, AutoKeyBacking::Blue);
    assert!(matches!(key.despill, WireKeyDespill::Blue { .. }));
    assert!(key.blue.abs_diff(200) <= 2, "{key:?}");
    subject_kept(&frame, &key);
}

#[test]
fn green_spill_on_the_subject_raises_the_despill_strength() {
    let clean = Frame::new(160, 90, |_, _| [30, 190, 60]).with_subject();
    let mut spilled = Frame::new(160, 90, |_, _| [30, 190, 60]).with_subject();
    // A green cast over the shirt: green well above red and blue's mean.
    spilled.fill(160 * 3 / 8, 45, 160 * 5 / 8, 90, |_, _| [60, 96, 70]);
    let clean = auto_key(clean.picture(), AutoKeySample::Dominant).unwrap();
    let spilled_key = auto_key(spilled.picture(), AutoKeySample::Dominant).unwrap();
    let outside = 160 * 90 - spilled_key.agreeing;
    let tinted = (160 * 5 / 8 - 160 * 3 / 8) * 45;
    let strength = 128 + (127 * tinted + outside / 2) / outside;
    assert_eq!(
        spilled_key.despill,
        WireKeyDespill::Green {
            strength: u8::try_from(strength).unwrap()
        }
    );
    assert_eq!(u32::from(spilled_key.spill), strength);
    assert!(spilled_key.spill > clean.spill);
}

#[test]
fn eyedropper_keys_the_picked_colour_even_in_shadow() {
    // Two greens: a big bright one and a dark corner the operator picks.
    let mut frame = Frame::new(160, 90, |x, y| noisy([40, 200, 70], x, y, 3));
    frame.fill(0, 0, 40, 30, |x, y| noisy([20, 110, 40], x, y, 3));
    let frame = frame.with_subject();
    let key = auto_key(frame.picture(), AutoKeySample::Point { x: 10, y: 10 }).unwrap();
    assert_eq!(key.backing, AutoKeyBacking::Green);
    // The shadow is the same hue, so it joins the bright cluster, and the
    // similarity reaches the picked colour.
    let subject = u32::try_from(subject_pixels(&frame).len()).unwrap();
    assert_eq!(key.agreeing, 160 * 90 - subject, "{key:?}");
    let picked = [20, 110, 40];
    assert!(
        distance(picked, [key.red, key.green, key.blue]) <= key.similarity + 3,
        "{key:?}"
    );
    subject_kept(&frame, &key);
}

#[test]
fn eyedropper_accepts_any_saturated_colour_without_despill() {
    let mut frame = Frame::new(64, 36, |_, _| [30, 190, 60]);
    frame.fill(0, 0, 16, 16, |_, _| [200, 30, 40]);
    let key = auto_key(frame.picture(), AutoKeySample::Point { x: 3, y: 3 }).unwrap();
    assert_eq!(key.backing, AutoKeyBacking::Other);
    assert_eq!([key.red, key.green, key.blue], [200, 30, 40]);
    assert_eq!(key.despill, WireKeyDespill::Off);
    assert_eq!(key.agreeing, 16 * 16);
}

#[test]
fn eyedropper_window_is_clamped_at_the_corner() {
    let frame = Frame::new(8, 8, |_, _| [10, 50, 220]);
    let key = auto_key(frame.picture(), AutoKeySample::Point { x: 7, y: 7 }).unwrap();
    assert_eq!(key.backing, AutoKeyBacking::Blue);
    assert_eq!([key.red, key.green, key.blue], [10, 50, 220]);
}

#[test]
fn grey_picks_and_pictures_without_a_backing_are_refused() {
    let grey = Frame::new(32, 18, |x, _| {
        let level = u8::try_from(x * 8).unwrap();
        [level, level, level]
    });
    assert!(matches!(
        auto_key(grey.picture(), AutoKeySample::Point { x: 4, y: 4 }),
        Err(AutoKeyError::Unsaturated { .. })
    ));
    assert!(matches!(
        auto_key(grey.picture(), AutoKeySample::Dominant),
        Err(AutoKeyError::NoBacking { agreeing: 0, .. })
    ));
    // A little green in a grey picture is not a backing.
    let mut sparse = Frame::new(32, 18, |_, _| [128, 128, 128]);
    sparse.fill(0, 0, 4, 4, |_, _| [0, 200, 0]);
    assert!(matches!(
        auto_key(sparse.picture(), AutoKeySample::Dominant),
        Err(AutoKeyError::NoBacking {
            agreeing: 16,
            samples: 576
        })
    ));
    assert_eq!(
        auto_key(grey.picture(), AutoKeySample::Point { x: 32, y: 0 }),
        Err(AutoKeyError::PointOutside {
            x: 32,
            y: 0,
            width: 32,
            height: 18
        })
    );
}

#[test]
fn bad_pictures_are_refused() {
    assert_eq!(
        AutoKeyPicture::new(0, 4, &[]).unwrap_err(),
        AutoKeyError::EmptyPicture
    );
    assert_eq!(
        AutoKeyPicture::new(2, 2, &[0; 15]).unwrap_err(),
        AutoKeyError::PictureSize {
            width: 2,
            height: 2,
            bytes: 15
        }
    );
}

#[test]
fn full_hd_frames_are_subsampled_deterministically() {
    let frame = Frame::new(1_920, 1_080, |x, y| noisy([35, 175, 65], x, y, 10)).with_subject();
    let first = auto_key(frame.picture(), AutoKeySample::Dominant).unwrap();
    let second = auto_key(frame.picture(), AutoKeySample::Dominant).unwrap();
    assert_eq!(first, second);
    let step = (1_920 * 1_080_u32).div_ceil(65_536);
    assert_eq!(first.samples, (1_920 * 1_080_u32).div_ceil(step));
    assert_eq!(first.backing, AutoKeyBacking::Green);
    subject_kept(&frame, &first);
}

#[test]
fn the_chroma_key_keeps_the_layers_matte_refinement() {
    let frame = Frame::new(16, 9, |_, _| [30, 190, 60]);
    let key = auto_key(frame.picture(), AutoKeySample::Dominant).unwrap();
    let previous = WireLayerKey::Luma(WireLumaKey {
        low: 10,
        high: 255,
        softness: 4,
        invert: false,
        refinement: WireKeyRefinement {
            clip_black: 12,
            clip_white: 240,
            choke: 2,
            edge_softness: 1,
            despill: WireKeyDespill::Blue { strength: 9 },
        },
    });
    let WireLayerKey::Chroma(chroma) = key.layer_key(Some(previous)) else {
        panic!("auto key is a chroma key");
    };
    assert_eq!(
        chroma.refinement,
        WireKeyRefinement {
            clip_black: 12,
            clip_white: 240,
            choke: 2,
            edge_softness: 1,
            despill: key.despill,
        }
    );
    assert_eq!(key.chroma_key(None).refinement.clip_white, 255);
    assert!(key.layer_key(None).is_valid());
}
