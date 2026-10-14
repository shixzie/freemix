use fm_video::{BlendError, FrameError, ImageFrame, Rgba8, crossfade, solid_color, write_ppm};

#[test]
fn crossfade_has_exact_endpoints_and_rounded_midpoint() {
    let left = solid_color(1, 1, Rgba8::new(0, 10, 20, 30)).unwrap();
    let right = solid_color(1, 1, Rgba8::new(255, 110, 220, 230)).unwrap();

    assert_eq!(crossfade(&left, &right, 0, 2).unwrap(), left);
    assert_eq!(crossfade(&left, &right, 2, 2).unwrap(), right);
    assert_eq!(
        crossfade(&left, &right, 1, 2).unwrap().pixel(0, 0),
        Some(Rgba8::new(128, 60, 120, 130))
    );
}

#[test]
fn crossfade_reports_ratio_and_format_mismatches() {
    let one = solid_color(1, 1, Rgba8::default()).unwrap();
    let wide = solid_color(2, 1, Rgba8::default()).unwrap();
    let tall = solid_color(1, 2, Rgba8::default()).unwrap();
    let padded = ImageFrame::new(1, 1, 8, vec![0; 8]).unwrap();

    assert_eq!(
        crossfade(&one, &one, 0, 0),
        Err(BlendError::ZeroDenominator)
    );
    assert_eq!(
        crossfade(&one, &one, 2, 1),
        Err(BlendError::NumeratorExceedsDenominator {
            numerator: 2,
            denominator: 1
        })
    );
    assert!(matches!(
        crossfade(&one, &wide, 1, 2),
        Err(BlendError::WidthMismatch { .. })
    ));
    assert!(matches!(
        crossfade(&one, &tall, 1, 2),
        Err(BlendError::HeightMismatch { .. })
    ));
    assert!(matches!(
        crossfade(&one, &padded, 1, 2),
        Err(BlendError::StrideMismatch { .. })
    ));
}

#[test]
fn frame_rejects_zero_dimensions_bad_stride_overflow_and_large_buffers() {
    assert_eq!(
        ImageFrame::new(0, 1, 0, Vec::new()),
        Err(FrameError::ZeroWidth)
    );
    assert_eq!(
        ImageFrame::new(1, 0, 4, Vec::new()),
        Err(FrameError::ZeroHeight)
    );
    assert_eq!(
        ImageFrame::new(2, 1, 4, vec![0; 4]),
        Err(FrameError::StrideTooSmall {
            minimum: 8,
            actual: 4
        })
    );
    assert_eq!(
        ImageFrame::new(1, u32::MAX, usize::MAX, Vec::new()),
        Err(FrameError::LayoutOverflow)
    );
    assert!(matches!(
        solid_color(100_000, 100_000, Rgba8::default()),
        Err(FrameError::BufferTooLarge { .. })
    ));
}

#[test]
fn ppm_writer_emits_valid_header_and_rgb_pixels_without_alpha_or_padding() {
    let frame =
        ImageFrame::new(2, 1, 12, vec![255, 0, 0, 7, 0, 128, 255, 9, 44, 44, 44, 44]).unwrap();
    let mut ppm = Vec::new();

    write_ppm(&frame, &mut ppm).unwrap();

    let mut expected = b"P6\n2 1\n255\n".to_vec();
    expected.extend_from_slice(&[255, 0, 0, 0, 128, 255]);
    assert_eq!(ppm, expected);
}
