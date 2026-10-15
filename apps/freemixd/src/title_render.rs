//! Title input rendering for the monitor tap, exact title previews, and
//! native live title sources.
//!
//! The renderer lives in `fm-title-render`, shared with `freemix-cli`'s
//! `title-render-frame`, so the daemon and the CLI load templates, fonts,
//! and image assets and draw production clocks the same way. `native-media`
//! builds enable its `images` feature (PNG and JPEG decoding through
//! `fm-codec-image`); other builds draw every image element as the
//! reference renderer's placeholder.

#[cfg(test)]
pub(crate) use fm_title_render::LoadedTitleTemplate;
#[cfg(feature = "native-media")]
pub(crate) use fm_title_render::unpremultiply;
pub(crate) use fm_title_render::{
    TitleAssets, TitleKey, TitleTime, field_value, flatten_over_black, model_field_value,
    render_title_at, render_title_preview, title_key, title_of_input,
};
