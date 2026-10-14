//! Deterministic CPU reference video primitives.

mod blend;
mod color;
mod composite;
mod frame;
mod freeze;
mod generate;
mod geometry;
mod png;
mod ppm;

pub use blend::{BlendError, crossfade};
pub use color::{ColorMatrix, ColorRange, Yuv8, rgb_to_yuv, yuv_to_rgb};
pub use composite::{
    CompositeError, Layer, apply_opacity_premultiplied, compose_layers, draw_inset_rect_border,
    premultiply_alpha,
};
pub use frame::{FrameError, ImageFrame, Rgba8};
pub use freeze::{
    FreezeController, FreezeError, FreezeState, OutputFrame, OutputOrigin, SourceFrame,
};
pub use generate::{solid_color, vertical_color_bars};
pub use geometry::{
    CropError, CropRect, Rotation, Transform, TransformError, crop, scale_nearest,
    transform_nearest,
};
pub use png::{
    MAX_PNG_DIMENSION, MAX_PNG_RAW_BYTES, PNG_SIGNATURE, PngBitDepth, PngEncodeError,
    STORED_BLOCK_MAX, adler32, crc32, encode_png_rgba8, encode_png_rgba16,
};
pub use ppm::write_ppm;
