//! SW-016 reference slice: lossless still snapshots and freeze/live-pause.

use fm_video::{
    BarsColorimetry, BarsPattern, FreezeController, FreezeError, FreezeState, ImageFrame,
    MAX_PNG_RAW_BYTES, OutputFrame, OutputOrigin, PNG_SIGNATURE, PngEncodeError, Rgba8,
    STORED_BLOCK_MAX, SourceFrame, TestSignalSpec, adler32, crc32, encode_png_rgba8,
    encode_png_rgba16, render_test_signal_rgba8, solid_color,
};

// ---------------------------------------------------------------------------
// Independent PNG verifier (bitwise CRC, stored-block inflate, filter 0).
// ---------------------------------------------------------------------------

fn bitwise_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn naive_adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1_u64, 0_u64);
    for &byte in bytes {
        a = (a + u64::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    u32::try_from((b << 16) | a).unwrap()
}

#[derive(Debug, PartialEq, Eq)]
struct DecodedPng {
    width: u32,
    height: u32,
    bit_depth: u8,
    /// Unfiltered scanline bytes, rows concatenated.
    pixels: Vec<u8>,
    stored_blocks: usize,
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().unwrap())
}

fn decode_stored_png(png: &[u8]) -> DecodedPng {
    assert_eq!(&png[..8], &PNG_SIGNATURE);
    let mut cursor = 8;
    let mut chunks = Vec::new();
    while cursor < png.len() {
        let length = usize::try_from(be32(&png[cursor..cursor + 4])).unwrap();
        let kind: [u8; 4] = png[cursor + 4..cursor + 8].try_into().unwrap();
        let data = &png[cursor + 8..cursor + 8 + length];
        let crc = be32(&png[cursor + 8 + length..cursor + 12 + length]);
        assert_eq!(
            crc,
            bitwise_crc32(&png[cursor + 4..cursor + 8 + length]),
            "chunk CRC"
        );
        chunks.push((kind, data.to_vec()));
        cursor += 12 + length;
    }
    assert_eq!(cursor, png.len());
    let kinds: Vec<_> = chunks.iter().map(|(kind, _)| kind).collect();
    assert_eq!(kinds, [b"IHDR", b"IDAT", b"IEND"]);

    let header = &chunks[0].1;
    assert_eq!(header.len(), 13);
    let width = be32(&header[0..4]);
    let height = be32(&header[4..8]);
    let bit_depth = header[8];
    assert_eq!(
        &header[9..13],
        &[6, 0, 0, 0],
        "RGBA, deflate, filter 0, no interlace"
    );
    assert_eq!(chunks[2].1, [0_u8; 0], "empty IEND");

    let zlib = &chunks[1].1;
    assert_eq!((u16::from(zlib[0]) << 8 | u16::from(zlib[1])) % 31, 0);
    assert_eq!(zlib[0] & 0x0f, 8, "deflate method");
    assert_eq!(zlib[1] & 0x20, 0, "no preset dictionary");
    let mut raw = Vec::new();
    let mut position = 2;
    let mut stored_blocks = 0;
    loop {
        let header = zlib[position];
        assert_eq!(header & 0b110, 0, "only stored blocks");
        let length = u16::from_le_bytes([zlib[position + 1], zlib[position + 2]]);
        let inverse = u16::from_le_bytes([zlib[position + 3], zlib[position + 4]]);
        assert_eq!(length, !inverse);
        let start = position + 5;
        let end = start + usize::from(length);
        raw.extend_from_slice(&zlib[start..end]);
        position = end;
        stored_blocks += 1;
        if header & 1 == 1 {
            break;
        }
    }
    assert_eq!(position + 4, zlib.len(), "adler trailer ends the stream");
    assert_eq!(be32(&zlib[position..]), naive_adler32(&raw));

    let bytes_per_pixel = usize::from(bit_depth / 8) * 4;
    let row = usize::try_from(width).unwrap() * bytes_per_pixel;
    assert_eq!(raw.len(), (row + 1) * usize::try_from(height).unwrap());
    let mut pixels = Vec::new();
    for scanline in raw.chunks(row + 1) {
        assert_eq!(scanline[0], 0, "filter type None");
        pixels.extend_from_slice(&scanline[1..]);
    }
    DecodedPng {
        width,
        height,
        bit_depth,
        pixels,
        stored_blocks,
    }
}

// ---------------------------------------------------------------------------
// Checksums and PNG encoding.
// ---------------------------------------------------------------------------

#[test]
fn checksums_match_standard_check_values() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    assert_eq!(crc32(b""), 0);
    assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    assert_eq!(adler32(b""), 1);
    let long: Vec<u8> = (0..200_000_u32)
        .map(|i| (i * 7 + 3).to_le_bytes()[0])
        .collect();
    assert_eq!(crc32(&long), bitwise_crc32(&long));
    assert_eq!(adler32(&long), naive_adler32(&long));
    let saturated = vec![0xff_u8; 100_000];
    assert_eq!(adler32(&saturated), naive_adler32(&saturated));
}

#[test]
fn one_pixel_rgba8_png_matches_golden_bytes() {
    let frame = solid_color(1, 1, Rgba8::new(1, 2, 3, 4)).unwrap();
    // Cross-checked against Python's zlib.crc32/adler32/decompress.
    let golden: [u8; 73] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x10, 0x49, 0x44, 0x41, 0x54, 0x78, 0x01, 0x01, 0x05,
        0x00, 0xfa, 0xff, 0x00, 0x01, 0x02, 0x03, 0x04, 0x00, 0x19, 0x00, 0x0b, 0xb9, 0xb0, 0xe3,
        0xeb, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    assert_eq!(encode_png_rgba8(&frame).unwrap(), golden);
}

#[test]
fn rgba8_png_round_trips_exactly_and_ignores_row_padding() {
    let bars = render_test_signal_rgba8(
        TestSignalSpec {
            pattern: BarsPattern::Bars75,
            colorimetry: BarsColorimetry::Bt709,
        },
        8,
        3,
    )
    .unwrap();
    let decoded = decode_stored_png(&encode_png_rgba8(&bars).unwrap());
    assert_eq!(
        (decoded.width, decoded.height, decoded.bit_depth),
        (8, 3, 8)
    );
    assert_eq!(decoded.pixels, bars.pixels());

    // 2x2 frame with 4 bytes of padding per row; padding must not leak.
    let padded = ImageFrame::new(
        2,
        2,
        12,
        vec![
            10, 11, 12, 13, 20, 21, 22, 23, 0xee, 0xee, 0xee, 0xee, //
            30, 31, 32, 33, 40, 41, 42, 43, 0xee, 0xee, 0xee, 0xee,
        ],
    )
    .unwrap();
    let decoded = decode_stored_png(&encode_png_rgba8(&padded).unwrap());
    assert_eq!(
        decoded.pixels,
        [
            10, 11, 12, 13, 20, 21, 22, 23, 30, 31, 32, 33, 40, 41, 42, 43
        ]
    );
    // Deterministic: identical input, identical bytes.
    assert_eq!(
        encode_png_rgba8(&padded).unwrap(),
        encode_png_rgba8(&padded).unwrap()
    );
}

#[test]
fn rgba16_png_is_big_endian_and_lossless() {
    let samples: Vec<u16> = vec![
        0x0102, 0xa0b0, 0xffff, 0x0000, 0x1234, 0x5678, 0x9abc, 0xdef0,
    ];
    let decoded = decode_stored_png(&encode_png_rgba16(2, 1, &samples).unwrap());
    assert_eq!(
        (decoded.width, decoded.height, decoded.bit_depth),
        (2, 1, 16)
    );
    let restored: Vec<u16> = decoded
        .pixels
        .chunks(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    assert_eq!(restored, samples);
}

#[test]
fn stored_blocks_split_exactly_at_block_limit() {
    // Filtered row = 1 + 4 * width bytes. Choose sizes landing exactly on,
    // just under, and just over the 65 535-byte stored-block limit.
    // 3 rows of (1 + 4 * 5461) = 3 * 21845 = 65 535 bytes: exactly one block.
    let exact = solid_color(5461, 3, Rgba8::new(9, 8, 7, 6)).unwrap();
    let decoded = decode_stored_png(&encode_png_rgba8(&exact).unwrap());
    assert_eq!(decoded.stored_blocks, 1);
    assert_eq!(decoded.pixels, exact.pixels());

    // One more row pushes 21 845 bytes into a second block.
    let over = solid_color(5461, 4, Rgba8::new(1, 1, 1, 1)).unwrap();
    let decoded = decode_stored_png(&encode_png_rgba8(&over).unwrap());
    assert_eq!(decoded.stored_blocks, 2);
    assert_eq!(decoded.pixels, over.pixels());

    // Large 16-bit image spanning many blocks.
    let width = 300_u32;
    let height = 120_u32;
    let samples: Vec<u16> = (0..width * height * 4)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 16) as u16)
        .collect();
    let decoded = decode_stored_png(&encode_png_rgba16(width, height, &samples).unwrap());
    let raw_len: usize = (1 + 8 * 300) * 120;
    assert_eq!(decoded.stored_blocks, raw_len.div_ceil(STORED_BLOCK_MAX));
    let restored: Vec<u16> = decoded
        .pixels
        .chunks(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    assert_eq!(restored, samples);
}

#[test]
fn png_encoder_reports_explicit_errors() {
    assert_eq!(encode_png_rgba16(0, 1, &[]), Err(PngEncodeError::ZeroWidth));
    assert_eq!(
        encode_png_rgba16(1, 0, &[]),
        Err(PngEncodeError::ZeroHeight)
    );
    assert_eq!(
        encode_png_rgba16(2, 1, &[0; 7]),
        Err(PngEncodeError::SampleCountMismatch {
            expected: 8,
            actual: 7
        })
    );
    assert_eq!(
        encode_png_rgba16(0x8000_0000, 1, &[]),
        Err(PngEncodeError::DimensionTooLarge {
            actual: 0x8000_0000,
            maximum: 0x7fff_ffff
        })
    );
    // 16384 * 8 + 1 = 131 073 bytes per row; 4097 rows exceeds 512 MiB.
    assert_eq!(
        encode_png_rgba16(16_384, 4097, &[]),
        Err(PngEncodeError::RawDataTooLarge {
            required: 131_073 * 4097,
            maximum: MAX_PNG_RAW_BYTES
        })
    );
}

// ---------------------------------------------------------------------------
// Freeze / live-pause.
// ---------------------------------------------------------------------------

const D: i64 = 1000;

fn src(index: u64, pts: i64) -> SourceFrame<u64> {
    SourceFrame::new(index, pts, index)
}

fn summary(frame: &OutputFrame<u64>) -> (i64, u64, OutputOrigin) {
    (frame.pts, frame.payload, frame.origin)
}

/// Pushes one source frame then emits one output slot.
fn step(
    controller: &mut FreezeController<u64>,
    index: u64,
    pts: i64,
) -> Option<(i64, u64, OutputOrigin)> {
    controller.push_source(src(index, pts)).unwrap();
    controller.emit().unwrap().as_ref().map(summary)
}

#[test]
fn freeze_latches_exact_boundary_repeats_and_resumes_without_regression() {
    use OutputOrigin::{Frozen, Live};
    let mut controller = FreezeController::new(D).unwrap();
    assert_eq!(step(&mut controller, 0, 5_000), Some((5_000, 0, Live)));
    assert_eq!(step(&mut controller, 1, 6_000), Some((6_000, 1, Live)));

    controller.request_freeze(3).unwrap();
    assert_eq!(
        controller.state(),
        FreezeState::FreezePending { at_index: 3 }
    );
    // Frame 2 precedes the boundary and still passes live.
    assert_eq!(step(&mut controller, 2, 7_000), Some((7_000, 2, Live)));
    // Frame 3 is latched and presented as the first frozen output.
    assert_eq!(step(&mut controller, 3, 8_000), Some((8_000, 3, Frozen)));
    assert_eq!(controller.state(), FreezeState::Frozen { latched_index: 3 });
    // Live frames keep arriving but output repeats the latched payload.
    assert_eq!(step(&mut controller, 4, 9_000), Some((9_000, 3, Frozen)));
    assert_eq!(step(&mut controller, 5, 10_000), Some((10_000, 3, Frozen)));
    // Output keeps ticking even when the source stalls during freeze.
    assert_eq!(
        controller.emit().unwrap().as_ref().map(summary),
        Some((11_000, 3, Frozen))
    );
    assert_eq!(
        controller.emit().unwrap().as_ref().map(summary),
        Some((12_000, 3, Frozen))
    );

    controller.resume().unwrap();
    assert_eq!(controller.state(), FreezeState::Live);
    // Newest live frame (5, PTS 10 000) maps to 10 000, which would regress
    // behind 12 000, so the rule lifts it to last + D.
    assert_eq!(
        controller.emit().unwrap().as_ref().map(summary),
        Some((13_000, 5, Live))
    );
    // Later live frames follow max(map(pts), last + D).
    assert_eq!(step(&mut controller, 6, 11_000), Some((14_000, 6, Live)));
    // A source jump ahead of output yields a forward step, never backward.
    assert_eq!(step(&mut controller, 7, 20_000), Some((20_000, 7, Live)));
    // Frame 4 was superseded by 5 during the freeze.
    assert_eq!(controller.dropped_source_frames(), 1);
}

#[test]
fn freeze_latches_first_frame_after_a_skipped_boundary() {
    let mut controller = FreezeController::new(D).unwrap();
    step(&mut controller, 10, 0);
    controller.request_freeze(12).unwrap();
    // Index 12 never arrives; 13 is the first frame at or after the request.
    assert_eq!(
        step(&mut controller, 13, 3_000),
        Some((3_000, 13, OutputOrigin::Frozen))
    );
    assert_eq!(
        controller.state(),
        FreezeState::Frozen { latched_index: 13 }
    );
}

#[test]
fn output_pts_rounds_onto_cadence_grid_and_is_strictly_increasing() {
    let mut controller = FreezeController::new(D).unwrap();
    // Origin is the first source PTS (250); off-grid PTS round up.
    assert_eq!(step(&mut controller, 0, 250).unwrap().0, 250);
    assert_eq!(step(&mut controller, 1, 1_100).unwrap().0, 1_250);
    assert_eq!(step(&mut controller, 2, 1_300).unwrap().0, 2_250);
    assert_eq!(step(&mut controller, 3, 3_250).unwrap().0, 3_250);
    assert_eq!(controller.live_output_pts(-750), Ok(4_250));

    // Mixed freeze/resume cycles never regress.
    let mut last = controller.last_output_pts().unwrap();
    let mut pts = 3_250;
    for index in 4..200_u64 {
        pts += if index % 7 == 0 { 2_500 } else { 400 };
        controller.push_source(src(index, pts)).unwrap();
        if index % 23 == 0 {
            controller.request_freeze(index + 2).unwrap();
        }
        if index % 23 == 9 && matches!(controller.state(), FreezeState::Frozen { .. }) {
            controller.resume().unwrap();
        }
        if let Some(out) = controller.emit().unwrap() {
            assert!(out.pts >= last + D, "{} after {last}", out.pts);
            assert_eq!((out.pts - 250).rem_euclid(D), 0, "on grid");
            last = out.pts;
        }
    }
}

#[test]
fn live_emit_without_new_frame_returns_none_and_resume_waits_for_live() {
    let mut controller = FreezeController::new(D).unwrap();
    assert_eq!(controller.emit(), Ok(None));
    step(&mut controller, 0, 0);
    assert_eq!(controller.emit(), Ok(None));
    controller.request_freeze(1).unwrap();
    step(&mut controller, 1, 1_000);
    controller.resume().unwrap();
    // No live frame newer than the latched one: output waits.
    assert_eq!(controller.emit(), Ok(None));
    assert_eq!(
        step(&mut controller, 2, 2_000),
        Some((2_000, 2, OutputOrigin::Live))
    );
}

#[test]
fn freeze_controller_reports_explicit_errors_without_state_change() {
    assert_eq!(
        FreezeController::<u64>::new(0).err(),
        Some(FreezeError::NonPositiveFrameDuration { duration: 0 })
    );
    let mut controller = FreezeController::new(D).unwrap();
    assert_eq!(controller.resume(), Err(FreezeError::NotFrozen));
    assert_eq!(
        controller.cancel_freeze(),
        Err(FreezeError::NoPendingFreeze)
    );
    controller.push_source(src(5, 100)).unwrap();
    assert_eq!(
        controller.push_source(src(5, 200)),
        Err(FreezeError::SourceIndexNotIncreasing {
            previous: 5,
            actual: 5
        })
    );
    assert_eq!(
        controller.push_source(src(6, 100)),
        Err(FreezeError::SourcePtsNotIncreasing {
            previous: 100,
            actual: 100
        })
    );
    assert_eq!(
        controller.request_freeze(5),
        Err(FreezeError::BoundaryAlreadyPassed {
            requested: 5,
            last_received: 5
        })
    );
    controller.request_freeze(8).unwrap();
    assert_eq!(
        controller.request_freeze(9),
        Err(FreezeError::FreezeAlreadyPending { at_index: 8 })
    );
    assert_eq!(controller.resume(), Err(FreezeError::NotFrozen));
    controller.cancel_freeze().unwrap();
    assert_eq!(controller.state(), FreezeState::Live);
    controller.request_freeze(6).unwrap();
    controller.push_source(src(6, 200)).unwrap();
    assert_eq!(
        controller.request_freeze(7),
        Err(FreezeError::AlreadyFrozen { latched_index: 6 })
    );

    let mut overflow = FreezeController::new(i64::MAX).unwrap();
    overflow.push_source(src(0, i64::MAX - 1)).unwrap();
    overflow.emit().unwrap();
    overflow.request_freeze(1).unwrap();
    overflow.push_source(src(1, i64::MAX)).unwrap();
    assert_eq!(overflow.emit(), Err(FreezeError::TimestampOverflow));
    assert_eq!(overflow.last_output_pts(), Some(i64::MAX - 1));
}

#[test]
fn frozen_snapshot_of_latched_payload_is_lossless() {
    // Payload handles can be shared frames; the latched frame encodes exactly.
    let frames: Vec<std::sync::Arc<ImageFrame>> = (0..4_u8)
        .map(|i| std::sync::Arc::new(solid_color(3, 2, Rgba8::new(i, 2 * i, 3 * i, 255)).unwrap()))
        .collect();
    let mut controller = FreezeController::new(D).unwrap();
    controller.request_freeze(2).unwrap();
    let mut last = None;
    for (index, frame) in frames.iter().enumerate() {
        let index = index as u64;
        controller
            .push_source(SourceFrame::new(
                index,
                i64::try_from(index).unwrap() * D,
                frame.clone(),
            ))
            .unwrap();
        last = controller.emit().unwrap();
    }
    let out = last.unwrap();
    assert_eq!(out.origin, OutputOrigin::Frozen);
    assert!(std::sync::Arc::ptr_eq(&out.payload, &frames[2]));
    let decoded = decode_stored_png(&encode_png_rgba8(&out.payload).unwrap());
    assert_eq!(decoded.pixels, frames[2].pixels());
}
