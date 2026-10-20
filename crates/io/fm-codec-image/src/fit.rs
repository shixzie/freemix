//! Placement of a decoded still into an output raster.
//!
//! The picture fills exactly the destination rectangle
//! [`OutputScaling::viewport`] returns, cropped to the output; every other
//! output pixel is fully transparent black. Equal-size placements copy
//! pixels exactly. Everything else is resampled with a separable Catmull-Rom
//! filter (widened when minifying so every source pixel contributes) in
//! premultiplied alpha, so transparent neighbours never bleed colour into
//! visible edges, and then converted back to straight alpha. Only output
//! pixels inside the visible rectangle and the source rows and columns they
//! read are processed; large outputs are split across a few threads.

use std::ops::Range;

use fm_frame::{CpuVideoFrame, CpuVideoPayload, CpuVideoPlane, PixelFormat, VideoDimensions};
use fm_types::{OutputScaling, ScaledViewport};

use crate::StillDecodeError;

/// Catmull-Rom support radius in filter units.
const FILTER_RADIUS: f64 = 2.0;
/// Output rows per worker below which splitting is not worth a thread.
const MIN_ROWS_PER_WORKER: usize = 64;
const MAX_WORKERS: usize = 8;

/// Places an RGBA8 `frame` into an `output` raster with `scaling`.
///
/// Returns a tightly packed straight-alpha RGBA8 frame of exactly `output`
/// size, with the input's timing and metadata. The picture occupies exactly
/// the rectangle `scaling.viewport(..)` returns (cropped where it extends
/// past the output) and the rest is `(0, 0, 0, 0)`. When the rectangle has
/// the picture's own size (1:1, or any policy with equal sizes) the visible
/// pixels are exact copies; otherwise they are resampled in premultiplied
/// alpha.
///
/// # Errors
///
/// Returns [`StillDecodeError::FitUnsupportedPixelFormat`] for non-RGBA8
/// input, [`StillDecodeError::FitOutputTooLarge`] when `output` exceeds the
/// CPU payload limits, [`StillDecodeError::FitViewportOverflow`] when the
/// placement rectangle does not fit `u32`, or a frame-layout error.
pub fn fit_still(
    frame: &CpuVideoFrame,
    output: VideoDimensions,
    scaling: OutputScaling,
) -> Result<CpuVideoFrame, StillDecodeError> {
    let payload = frame.payload();
    if payload.format() != PixelFormat::Rgba8 {
        return Err(StillDecodeError::FitUnsupportedPixelFormat {
            format: payload.format(),
        });
    }
    let plane = payload
        .plane(0)
        .ok_or(StillDecodeError::FitUnsupportedPixelFormat {
            format: payload.format(),
        })?;
    let (target_width, target_height) = (output.width(), output.height());
    let too_large = StillDecodeError::FitOutputTooLarge {
        width: target_width,
        height: target_height,
    };
    if target_width > CpuVideoPayload::MAX_WIDTH || target_height > CpuVideoPayload::MAX_HEIGHT {
        return Err(too_large);
    }
    let row_bytes = target_width as usize * 4;
    let total_bytes = row_bytes
        .checked_mul(target_height as usize)
        .filter(|bytes| *bytes <= CpuVideoPayload::MAX_TOTAL_BYTES)
        .ok_or(too_large)?;

    let source = Source {
        bytes: plane.bytes(),
        stride: plane.stride(),
        width: payload.dimensions().width(),
        height: payload.dimensions().height(),
    };
    let viewport = scaling
        .viewport(source.width, source.height, target_width, target_height)
        .ok_or(StillDecodeError::FitViewportOverflow)?;

    let mut pixels = vec![0; total_bytes];
    if let (Some(columns), Some(rows)) = (
        visible_span(viewport.x, viewport.width, target_width),
        visible_span(viewport.y, viewport.height, target_height),
    ) {
        let region = Region {
            columns,
            rows,
            row_bytes,
        };
        if viewport.width == source.width && viewport.height == source.height {
            copy_exact(&source, &region, &mut pixels);
        } else {
            resample(&source, viewport, &region, &mut pixels);
        }
    }

    let plane = CpuVideoPlane::new(row_bytes, pixels).map_err(StillDecodeError::FramePayload)?;
    let payload = CpuVideoPayload::new(PixelFormat::Rgba8, output, vec![plane])
        .map_err(StillDecodeError::FramePayload)?;
    let fitted = CpuVideoFrame::new(frame.timing(), payload);
    match frame.metadata() {
        Some(metadata) => fitted
            .with_metadata(metadata)
            .map_err(StillDecodeError::FrameMetadata),
        None => Ok(fitted),
    }
}

struct Source<'a> {
    bytes: &'a [u8],
    stride: usize,
    width: u32,
    height: u32,
}

impl Source<'_> {
    fn row(&self, y: usize) -> &[u8] {
        let start = y * self.stride;
        &self.bytes[start..start + self.width as usize * 4]
    }
}

/// The visible part of the viewport along one axis.
#[derive(Clone, Copy)]
struct Span {
    /// First visible output coordinate.
    output_start: usize,
    /// Number of visible output pixels.
    count: usize,
    /// Viewport-local coordinate of the first visible pixel.
    local_start: usize,
}

struct Region {
    columns: Span,
    rows: Span,
    row_bytes: usize,
}

fn visible_span(origin: i32, extent: u32, target: u32) -> Option<Span> {
    let origin = i64::from(origin);
    let start = origin.max(0);
    let end = (origin + i64::from(extent)).min(i64::from(target));
    (end > start).then(|| Span {
        output_start: usize::try_from(start).unwrap_or(0),
        count: usize::try_from(end - start).unwrap_or(0),
        local_start: usize::try_from(start - origin).unwrap_or(0),
    })
}

fn copy_exact(source: &Source<'_>, region: &Region, pixels: &mut [u8]) {
    let columns = region.columns;
    let source_x = columns.local_start * 4;
    let length = columns.count * 4;
    for row in 0..region.rows.count {
        let source_row = source.row(region.rows.local_start + row);
        let start = (region.rows.output_start + row) * region.row_bytes + columns.output_start * 4;
        pixels[start..start + length].copy_from_slice(&source_row[source_x..source_x + length]);
    }
}

/// Normalised filter taps for every visible output pixel along one axis.
struct AxisWeights {
    /// First source index read by each output pixel.
    starts: Vec<usize>,
    /// Number of taps read by each output pixel.
    lengths: Vec<usize>,
    /// `taps` weights per output pixel, zero padded.
    weights: Vec<f32>,
    taps: usize,
}

impl AxisWeights {
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "coordinates are bounded raster sizes far inside f64 precision"
    )]
    fn new(source_len: u32, viewport_len: u32, span: Span) -> Self {
        let scale = f64::from(source_len) / f64::from(viewport_len);
        let filter_scale = scale.max(1.0);
        let support = FILTER_RADIUS * filter_scale;
        let taps = (support * 2.0).ceil() as usize + 1;
        let source_len = source_len as usize;
        let mut starts = Vec::with_capacity(span.count);
        let mut lengths = Vec::with_capacity(span.count);
        let mut weights = vec![0.0f32; span.count * taps];
        let mut exact = vec![0.0f64; taps];
        for (index, row) in weights.chunks_exact_mut(taps).enumerate() {
            let center = (span.local_start as f64 + index as f64 + 0.5) * scale;
            let first = ((center - support).floor().max(0.0) as usize).min(source_len - 1);
            let last = ((center + support).ceil().max(0.0) as usize)
                .min(source_len)
                .min(first + taps)
                .max(first + 1);
            let exact = &mut exact[..last - first];
            for (slot, source_index) in exact.iter_mut().zip(first..last) {
                *slot = catmull_rom((source_index as f64 + 0.5 - center) / filter_scale);
            }
            let mut sum: f64 = exact.iter().sum();
            if sum.abs() < 1e-6 {
                // Degenerate window: sample the nearest source pixel.
                exact.fill(0.0);
                let nearest = (center.floor().max(0.0) as usize).clamp(first, last - 1);
                exact[nearest - first] = 1.0;
                sum = 1.0;
            }
            for (slot, weight) in row.iter_mut().zip(exact.iter()) {
                *slot = (weight / sum) as f32;
            }
            starts.push(first);
            lengths.push(last - first);
        }
        // Trim to the widest window actually used.
        let used = lengths.iter().copied().max().unwrap_or(1);
        let weights = if used == taps {
            weights
        } else {
            weights
                .chunks_exact(taps)
                .flat_map(|row| row[..used].iter().copied())
                .collect()
        };
        Self {
            starts,
            lengths,
            weights,
            taps: used,
        }
    }

    fn taps_for(&self, index: usize) -> (usize, &[f32]) {
        let offset = index * self.taps;
        (
            self.starts[index],
            &self.weights[offset..offset + self.lengths[index]],
        )
    }
}

fn catmull_rom(x: f64) -> f64 {
    let x = x.abs();
    if x < 1.0 {
        (1.5 * x - 2.5) * x * x + 1.0
    } else if x < 2.0 {
        ((-0.5 * x + 2.5) * x - 4.0) * x + 2.0
    } else {
        0.0
    }
}

fn resample(source: &Source<'_>, viewport: ScaledViewport, region: &Region, pixels: &mut [u8]) {
    let horizontal = AxisWeights::new(source.width, viewport.width, region.columns);
    let vertical = AxisWeights::new(source.height, viewport.height, region.rows);
    let context = Resampler {
        source,
        horizontal: &horizontal,
        vertical: &vertical,
        columns: region.columns,
    };

    let rows = region.rows;
    let first_byte = rows.output_start * region.row_bytes;
    let visible = &mut pixels[first_byte..first_byte + rows.count * region.row_bytes];
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(MAX_WORKERS)
        .min(rows.count / MIN_ROWS_PER_WORKER)
        .max(1);
    if workers == 1 {
        context.rows(0..rows.count, visible, region.row_bytes);
        return;
    }
    let rows_per_worker = rows.count.div_ceil(workers);
    std::thread::scope(|scope| {
        for (worker, chunk) in visible
            .chunks_mut(rows_per_worker * region.row_bytes)
            .enumerate()
        {
            let context = &context;
            let start = worker * rows_per_worker;
            let count = chunk.len() / region.row_bytes;
            scope.spawn(move || context.rows(start..start + count, chunk, region.row_bytes));
        }
    });
}

struct Resampler<'a> {
    source: &'a Source<'a>,
    horizontal: &'a AxisWeights,
    vertical: &'a AxisWeights,
    columns: Span,
}

impl Resampler<'_> {
    /// Resamples visible output `rows` into `output`, whose first row is the
    /// first of `rows`.
    fn rows(&self, rows: Range<usize>, output: &mut [u8], row_bytes: usize) {
        let width = self.columns.count * 4;
        let capacity = self.vertical.taps;
        // Horizontally filtered source rows, keyed by `source_row % capacity`.
        // Vertical windows only move forward, so a window never needs a row
        // that has been overwritten.
        let mut ring = vec![0.0f32; capacity * width];
        let mut next_row = 0usize;
        let column_start = self.horizontal.starts.first().copied().unwrap_or(0);
        let column_end = self
            .horizontal
            .starts
            .iter()
            .zip(&self.horizontal.lengths)
            .map(|(start, length)| start + length)
            .max()
            .unwrap_or(column_start);
        let mut premultiplied = vec![0.0f32; (column_end - column_start) * 4];
        let mut accumulator = vec![0.0f32; width];

        for (row_index, output_row) in rows.zip(output.chunks_exact_mut(row_bytes)) {
            let (window_start, weights) = self.vertical.taps_for(row_index);
            let window_end = window_start + weights.len();
            for source_row in next_row.max(window_start)..window_end {
                let slot = source_row % capacity;
                self.filter_row(
                    source_row,
                    column_start,
                    &mut premultiplied,
                    &mut ring[slot * width..(slot + 1) * width],
                );
            }
            next_row = next_row.max(window_end);

            accumulator.fill(0.0);
            for (offset, weight) in weights.iter().enumerate() {
                let slot = (window_start + offset) % capacity;
                let filtered = &ring[slot * width..(slot + 1) * width];
                for (sum, value) in accumulator.iter_mut().zip(filtered) {
                    *sum += weight * value;
                }
            }
            let start = self.columns.output_start * 4;
            store_straight(&accumulator, &mut output_row[start..start + width]);
        }
    }

    /// Premultiplies the used columns of one source row and filters it
    /// horizontally into `filtered`.
    fn filter_row(
        &self,
        source_row: usize,
        column_start: usize,
        premultiplied: &mut [f32],
        filtered: &mut [f32],
    ) {
        let row = self.source.row(source_row);
        let used = &row[column_start * 4..column_start * 4 + premultiplied.len()];
        for (target, pixel) in premultiplied
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(used.as_chunks::<4>().0)
        {
            let alpha = f32::from(pixel[3]);
            let scale = alpha / 255.0;
            target[0] = f32::from(pixel[0]) * scale;
            target[1] = f32::from(pixel[1]) * scale;
            target[2] = f32::from(pixel[2]) * scale;
            target[3] = alpha;
        }
        for (column, output) in filtered.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (start, weights) = self.horizontal.taps_for(column);
            let taps = &premultiplied[(start - column_start) * 4..];
            let mut sum = [0.0f32; 4];
            for (weight, pixel) in weights.iter().zip(taps.as_chunks::<4>().0) {
                sum[0] += weight * pixel[0];
                sum[1] += weight * pixel[1];
                sum[2] += weight * pixel[2];
                sum[3] += weight * pixel[3];
            }
            *output = sum;
        }
    }
}

/// Converts premultiplied float pixels back to straight-alpha RGBA8,
/// clamping filter overshoot so colour never exceeds alpha.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "values are clamped to 0..=255 before the cast"
)]
fn store_straight(premultiplied: &[f32], output: &mut [u8]) {
    for (pixel, target) in premultiplied
        .as_chunks::<4>()
        .0
        .iter()
        .zip(output.as_chunks_mut::<4>().0)
    {
        // Un-premultiply with the unclamped filtered alpha so negative-lobe
        // overshoot at an alpha edge keeps the colour ratio instead of
        // saturating it.
        let alpha = pixel[3];
        let alpha_byte = (alpha.clamp(0.0, 255.0) + 0.5) as u8;
        if alpha_byte == 0 {
            target.fill(0);
            continue;
        }
        let unscale = 255.0 / alpha;
        for channel in 0..3 {
            target[channel] = (pixel[channel].clamp(0.0, alpha) * unscale + 0.5).min(255.0) as u8;
        }
        target[3] = alpha_byte;
    }
}
