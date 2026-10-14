use std::sync::Arc;

use fm_types::{ColorPrimaries, MatrixCoefficients, SignalRange, TransferFunction};
use fm_video::{
    BarsColorimetry, BarsPattern, GeneratedContent, GeneratedSourceSpec, NominalRgb,
    PLUGE_LEVELS_PERCENT, Rgba8, StaticGenerator, TestSignalError, TestSignalSpec, YcbcrBitDepth,
    YcbcrCode, quantize_rgb8_full, quantize_ycbcr, render_test_signal_rgba8,
    render_test_signal_ycbcr444, test_signal_level,
};

const fn spec(pattern: BarsPattern, colorimetry: BarsColorimetry) -> TestSignalSpec {
    TestSignalSpec {
        pattern,
        colorimetry,
    }
}

fn codes(
    pattern: BarsPattern,
    colorimetry: BarsColorimetry,
    depth: YcbcrBitDepth,
) -> Vec<(u16, u16, u16)> {
    let planes = render_test_signal_ycbcr444(spec(pattern, colorimetry), 8, 1, depth).unwrap();
    (0..8)
        .map(|x| {
            let code = planes.sample(x, 0).unwrap();
            (code.y, code.cb, code.cr)
        })
        .collect()
}

/// SMPTE RP 219 / ITU-R BT.709 75 % bars, 10-bit narrow range.
#[test]
fn bt709_ten_bit_75_percent_bars_match_published_code_values() {
    assert_eq!(
        codes(
            BarsPattern::Bars75,
            BarsColorimetry::Bt709,
            YcbcrBitDepth::Ten
        ),
        [
            (721, 512, 512),
            (674, 176, 543),
            (581, 589, 176),
            (534, 253, 207),
            (251, 771, 817),
            (204, 435, 848),
            (111, 848, 481),
            (64, 512, 512),
        ]
    );
}

#[test]
fn bt709_ten_bit_100_percent_bars_match_published_code_values() {
    assert_eq!(
        codes(
            BarsPattern::Bars100,
            BarsColorimetry::Bt709,
            YcbcrBitDepth::Ten
        ),
        [
            (940, 512, 512),
            (877, 64, 553),
            (754, 615, 64),
            (691, 167, 105),
            (313, 857, 919),
            (250, 409, 960),
            (127, 960, 471),
            (64, 512, 512),
        ]
    );
}

#[test]
fn bt709_eight_bit_75_percent_bars() {
    assert_eq!(
        codes(
            BarsPattern::Bars75,
            BarsColorimetry::Bt709,
            YcbcrBitDepth::Eight
        ),
        [
            (180, 128, 128),
            (168, 44, 136),
            (145, 147, 44),
            (133, 63, 52),
            (63, 193, 204),
            (51, 109, 212),
            (28, 212, 120),
            (16, 128, 128),
        ]
    );
}

/// ITU-R BT.601 75 % bars, 8-bit narrow range.
#[test]
fn bt601_eight_bit_75_percent_bars_match_published_code_values() {
    assert_eq!(
        codes(
            BarsPattern::Bars75,
            BarsColorimetry::Bt601,
            YcbcrBitDepth::Eight
        ),
        [
            (180, 128, 128),
            (162, 44, 142),
            (131, 156, 44),
            (112, 72, 58),
            (84, 184, 198),
            (65, 100, 212),
            (35, 212, 114),
            (16, 128, 128),
        ]
    );
}

#[test]
fn bt601_eight_bit_100_percent_bars_match_published_code_values() {
    assert_eq!(
        codes(
            BarsPattern::Bars100,
            BarsColorimetry::Bt601,
            YcbcrBitDepth::Eight
        ),
        [
            (235, 128, 128),
            (210, 16, 146),
            (170, 166, 16),
            (145, 54, 34),
            (106, 202, 222),
            (81, 90, 240),
            (41, 240, 110),
            (16, 128, 128),
        ]
    );
}

#[test]
fn ebu_100_75_bars_have_full_white_and_75_percent_colours() {
    let ebu = codes(
        BarsPattern::Ebu100_75,
        BarsColorimetry::Bt709,
        YcbcrBitDepth::Ten,
    );
    let bars75 = codes(
        BarsPattern::Bars75,
        BarsColorimetry::Bt709,
        YcbcrBitDepth::Ten,
    );
    assert_eq!(ebu[0], (940, 512, 512));
    assert_eq!(ebu[1..], bars75[1..]);
}

#[test]
fn full_range_rgb_bars_are_exact() {
    let frame =
        render_test_signal_rgba8(spec(BarsPattern::Bars75, BarsColorimetry::Bt709), 8, 2).unwrap();
    let expected = [
        Rgba8::new(191, 191, 191, 255),
        Rgba8::new(191, 191, 0, 255),
        Rgba8::new(0, 191, 191, 255),
        Rgba8::new(0, 191, 0, 255),
        Rgba8::new(191, 0, 191, 255),
        Rgba8::new(191, 0, 0, 255),
        Rgba8::new(0, 0, 191, 255),
        Rgba8::new(0, 0, 0, 255),
    ];
    for y in 0..2 {
        for (x, color) in (0..8).zip(expected) {
            assert_eq!(frame.pixel(x, y), Some(color), "({x}, {y})");
        }
    }
    let full =
        render_test_signal_rgba8(spec(BarsPattern::Bars100, BarsColorimetry::Bt601), 8, 1).unwrap();
    assert_eq!(full.pixel(0, 0), Some(Rgba8::new(255, 255, 255, 255)));
    assert_eq!(full.pixel(4, 0), Some(Rgba8::new(255, 0, 255, 255)));
}

#[test]
fn bar_boundaries_are_exact_at_hd_width() {
    let width = 1920;
    for x in 0..width {
        let level = test_signal_level(BarsPattern::Bars75, width, 1, x, 0).unwrap();
        let expected = test_signal_level(BarsPattern::Bars75, 8, 1, x / 240, 0).unwrap();
        assert_eq!(level, expected, "column {x}");
    }
    // Seven bars over 1920 pixels: pixel x is in bar floor(7x/1920), so bar i
    // starts at ceil(1920i/7).
    let boundaries = [0_u32, 275, 549, 823, 1098, 1372, 1646];
    for (bar, start) in boundaries.iter().enumerate().skip(1) {
        let before = test_signal_level(BarsPattern::BarsPlugeRamp, width, 1080, start - 1, 0);
        let at = test_signal_level(BarsPattern::BarsPlugeRamp, width, 1080, *start, 0);
        assert_ne!(before, at, "bar {bar} starts at {start}");
    }
    assert_eq!(
        test_signal_level(BarsPattern::Bars75, width, 1, width, 0),
        None
    );
    assert_eq!(test_signal_level(BarsPattern::Bars75, 0, 1, 0, 0), None);
}

#[test]
fn pluge_and_ramp_bands_have_exact_levels() {
    let (width, height) = (9 * 4, 12);
    let planes = render_test_signal_ycbcr444(
        spec(BarsPattern::BarsPlugeRamp, BarsColorimetry::Bt709),
        width,
        height,
        YcbcrBitDepth::Ten,
    )
    .unwrap();
    // Rows 0..8 are bars, 8..9 ramp, 9..12 PLUGE.
    assert_eq!(planes.sample(0, 7), Some(YcbcrCode::new(721, 512, 512)));
    assert_eq!(
        planes.sample(width - 1, 7),
        Some(YcbcrCode::new(111, 848, 481))
    );
    assert_eq!(planes.sample(0, 8), Some(YcbcrCode::new(64, 512, 512)));
    assert_eq!(
        planes.sample(width - 1, 8),
        Some(YcbcrCode::new(940, 512, 512))
    );
    let pluge_luma: Vec<u16> = (0..9)
        .map(|column| planes.sample(column * 4, 9).unwrap().y)
        .collect();
    assert_eq!(pluge_luma, [64, 940, 64, 46, 64, 82, 64, 99, 64]);
    for column in 0..9 {
        for x in column * 4..column * 4 + 4 {
            for y in 9..12 {
                let sample = planes.sample(x, y).unwrap();
                assert_eq!(sample.y, pluge_luma[column as usize]);
                assert_eq!((sample.cb, sample.cr), (512, 512));
            }
        }
    }
    assert_eq!(PLUGE_LEVELS_PERCENT, [0, 100, 0, -2, 0, 2, 0, 4, 0]);

    let eight = render_test_signal_ycbcr444(
        spec(BarsPattern::BarsPlugeRamp, BarsColorimetry::Bt601),
        width,
        height,
        YcbcrBitDepth::Eight,
    )
    .unwrap();
    let pluge_luma_8: Vec<u16> = (0..9)
        .map(|column| eight.sample(column * 4, 11).unwrap().y)
        .collect();
    assert_eq!(pluge_luma_8, [16, 235, 16, 12, 16, 20, 16, 25, 16]);

    // Full-range RGB cannot carry sub-black: -2 % clips to 0, +2 % and +4 % are exact.
    let rgb = render_test_signal_rgba8(
        spec(BarsPattern::BarsPlugeRamp, BarsColorimetry::Bt709),
        width,
        height,
    )
    .unwrap();
    let pluge_rgb: Vec<u8> = (0..9)
        .map(|column| rgb.pixel(column * 4, 10).unwrap().r)
        .collect();
    assert_eq!(pluge_rgb, [0, 255, 0, 0, 0, 5, 0, 10, 0]);
}

#[test]
fn ramp_is_exact_per_column() {
    let (width, height) = (256, 4);
    let rgb = render_test_signal_rgba8(
        spec(BarsPattern::BarsPlugeRamp, BarsColorimetry::Bt709),
        width,
        height,
    )
    .unwrap();
    // height 4: bars rows [0, 2), ramp row [2, 3), PLUGE row [3, 4).
    for x in 0..width {
        let value = u8::try_from(x).unwrap();
        assert_eq!(rgb.pixel(x, 2), Some(Rgba8::new(value, value, value, 255)));
    }
    let planes = render_test_signal_ycbcr444(
        spec(BarsPattern::BarsPlugeRamp, BarsColorimetry::Bt709),
        width,
        height,
        YcbcrBitDepth::Ten,
    )
    .unwrap();
    let mut previous = 0;
    for x in 0..width {
        let sample = planes.sample(x, 2).unwrap();
        assert!(sample.y >= previous);
        assert_eq!((sample.cb, sample.cr), (512, 512));
        // DY = round((219·x/255 + 16)·4) = round(876x/255) + 64.
        let expected = (876 * x * 2 + 255) / (2 * 255) + 64;
        assert_eq!(u32::from(sample.y), expected, "column {x}");
        previous = sample.y;
    }
    let single = test_signal_level(BarsPattern::BarsPlugeRamp, 1, 4, 0, 2).unwrap();
    assert_eq!(single, NominalRgb::grey_percent(0));
}

#[test]
fn quantization_clips_to_permitted_codes() {
    let super_white = NominalRgb::percent(120, 120, 120);
    assert_eq!(
        quantize_ycbcr(super_white, BarsColorimetry::Bt709, YcbcrBitDepth::Ten).y,
        1_019
    );
    assert_eq!(
        quantize_ycbcr(super_white, BarsColorimetry::Bt709, YcbcrBitDepth::Eight).y,
        254
    );
    let deep_black = NominalRgb::percent(-20, -20, -20);
    assert_eq!(
        quantize_ycbcr(deep_black, BarsColorimetry::Bt601, YcbcrBitDepth::Ten).y,
        4
    );
    assert_eq!(
        quantize_ycbcr(deep_black, BarsColorimetry::Bt601, YcbcrBitDepth::Eight).y,
        1
    );
    assert_eq!(
        quantize_rgb8_full(super_white),
        Rgba8::new(255, 255, 255, 255)
    );
    assert_eq!(quantize_rgb8_full(deep_black), Rgba8::new(0, 0, 0, 255));
    assert_eq!(NominalRgb::new(1, 1, 1, 0), None);
    assert_eq!(
        NominalRgb::new(1, 2, 3, 4).unwrap().components(),
        (1, 2, 3, 4)
    );
}

#[test]
fn colorimetry_metadata_is_defined() {
    let hd = BarsColorimetry::Bt709.ycbcr_metadata();
    assert_eq!(hd.primaries, ColorPrimaries::Bt709);
    assert_eq!(hd.transfer, TransferFunction::Bt709);
    assert_eq!(hd.matrix, MatrixCoefficients::Bt709);
    assert_eq!(hd.range, SignalRange::Limited);
    let sd = BarsColorimetry::Bt601.ycbcr_metadata();
    assert_eq!(sd.primaries, ColorPrimaries::Bt601);
    assert_eq!(sd.matrix, MatrixCoefficients::Bt601);
    let rgb = BarsColorimetry::Bt709.rgb_metadata();
    assert_eq!(rgb.matrix, MatrixCoefficients::Identity);
    assert_eq!(rgb.range, SignalRange::Full);
    let planes = render_test_signal_ycbcr444(
        spec(BarsPattern::Bars75, BarsColorimetry::Bt601),
        2,
        2,
        YcbcrBitDepth::Eight,
    )
    .unwrap();
    assert_eq!(planes.metadata(), sd);
    assert_eq!((planes.width(), planes.height()), (2, 2));
    assert_eq!(planes.depth(), YcbcrBitDepth::Eight);
    assert_eq!(planes.y().len(), 4);
    assert_eq!(planes.cb().len(), 4);
    assert_eq!(planes.cr().len(), 4);
    assert_eq!(planes.sample(2, 0), None);
}

#[test]
fn invalid_rasters_are_rejected() {
    let bars = spec(BarsPattern::Bars75, BarsColorimetry::Bt709);
    assert!(matches!(
        render_test_signal_rgba8(bars, 0, 1),
        Err(TestSignalError::Frame(_))
    ));
    assert!(matches!(
        render_test_signal_ycbcr444(bars, 1, 0, YcbcrBitDepth::Ten),
        Err(TestSignalError::Frame(_))
    ));
    assert_eq!(
        render_test_signal_ycbcr444(bars, 65_535, 65_535, YcbcrBitDepth::Ten),
        Err(TestSignalError::PlaneTooLarge)
    );
}

#[test]
fn static_generator_renders_once_per_specification() {
    let mut generator = StaticGenerator::new();
    let bars = GeneratedSourceSpec {
        width: 64,
        height: 36,
        content: GeneratedContent::TestSignal(spec(
            BarsPattern::BarsPlugeRamp,
            BarsColorimetry::Bt709,
        )),
    };
    let first = generator.frame(bars).unwrap();
    for _ in 0..1_000 {
        let tick = generator.frame(bars).unwrap();
        assert!(Arc::ptr_eq(&first, &tick));
    }
    assert_eq!(generator.renders(), 1);
    assert_eq!(
        *first,
        render_test_signal_rgba8(
            spec(BarsPattern::BarsPlugeRamp, BarsColorimetry::Bt709),
            64,
            36
        )
        .unwrap()
    );

    let solid = GeneratedSourceSpec {
        width: 4,
        height: 2,
        content: GeneratedContent::Solid(Rgba8::new(10, 20, 30, 128)),
    };
    let colour = generator.frame(solid).unwrap();
    assert_eq!(generator.renders(), 2);
    assert_eq!(colour.pixel(3, 1), Some(Rgba8::new(10, 20, 30, 128)));
    assert!(Arc::ptr_eq(&colour, &generator.frame(solid).unwrap()));

    let invalid = GeneratedSourceSpec { width: 0, ..solid };
    assert!(generator.frame(invalid).is_err());
    assert_eq!(generator.renders(), 2);
    assert!(Arc::ptr_eq(&colour, &generator.frame(solid).unwrap()));
}
