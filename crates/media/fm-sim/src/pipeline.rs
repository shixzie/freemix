use crate::{PipelineConfigError, RegistryError, RenderError, SimulatedSource, SourcePattern};
use fm_switcher::{ProgramFrame, TransitionKind};
use fm_types::InputId;
use fm_video::Rgba8;
use fm_video::{
    FrameError, ImageFrame, TransitionShape, crossfade, render_test_signal_rgba8,
    render_transition, solid_color,
};
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct SimulatedPipeline {
    width: u32,
    height: u32,
    sources: BTreeMap<InputId, SimulatedSource>,
    /// Per-input video delays in frames (IN-022), ascending.
    video_delays: BTreeMap<InputId, u32>,
    /// The bars picture at the pipeline size, rendered on first use. The
    /// size never changes, so steady renders only copy it.
    bars: OnceLock<ImageFrame>,
}

impl SimulatedPipeline {
    /// Longest side of a landscape bound; a portrait pipeline swaps the axes.
    pub const MAX_WIDTH: u32 = 7_680;
    /// Most frames a simulated input may delay its picture. Equals the
    /// project model's video-delay bound.
    pub const MAX_VIDEO_DELAY_FRAMES: u32 = 600;
    /// Shortest side of a landscape bound; a portrait pipeline swaps the axes.
    pub const MAX_HEIGHT: u32 = 4_320;

    /// Creates an empty simulated pipeline with bounded output dimensions.
    ///
    /// The bound is orientation independent: a landscape or square canvas may
    /// be at most [`Self::MAX_WIDTH`]x[`Self::MAX_HEIGHT`], and a portrait
    /// canvas at most [`Self::MAX_HEIGHT`]x[`Self::MAX_WIDTH`], so a vertical
    /// 4320x7680 production is as valid as an 8K UHD landscape one.
    ///
    /// # Errors
    ///
    /// Returns a typed error for zero or out-of-range dimensions; the reported
    /// maximum is the bound for the requested orientation.
    pub fn new(width: u32, height: u32) -> Result<Self, PipelineConfigError> {
        if width == 0 {
            return Err(PipelineConfigError::ZeroWidth);
        }
        if height == 0 {
            return Err(PipelineConfigError::ZeroHeight);
        }
        let (maximum_width, maximum_height) = if height > width {
            (Self::MAX_HEIGHT, Self::MAX_WIDTH)
        } else {
            (Self::MAX_WIDTH, Self::MAX_HEIGHT)
        };
        if width > maximum_width || height > maximum_height {
            return Err(PipelineConfigError::DimensionsExceedLimit {
                width,
                height,
                maximum_width,
                maximum_height,
            });
        }
        Ok(Self {
            width,
            height,
            sources: BTreeMap::new(),
            video_delays: BTreeMap::new(),
            bars: OnceLock::new(),
        })
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Registers a source without replacing an existing identity.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::DuplicateSource`] if the input is registered.
    pub fn register(&mut self, source: SimulatedSource) -> Result<(), RegistryError> {
        if self.sources.contains_key(&source.input()) {
            return Err(RegistryError::DuplicateSource(source.input()));
        }
        self.sources.insert(source.input(), source);
        Ok(())
    }

    pub fn remove(&mut self, input: InputId) -> Option<SimulatedSource> {
        self.sources.remove(&input)
    }

    #[must_use]
    pub fn source(&self, input: InputId) -> Option<&SimulatedSource> {
        self.sources.get(&input)
    }

    /// Applies `input`'s video delay in frames (IN-022), clamped to
    /// [`Self::MAX_VIDEO_DELAY_FRAMES`]; zero clears it.
    pub fn set_video_delay(&mut self, input: InputId, frames: u32) {
        let frames = frames.min(Self::MAX_VIDEO_DELAY_FRAMES);
        if frames == 0 {
            self.video_delays.remove(&input);
        } else {
            self.video_delays.insert(input, frames);
        }
    }

    /// The delay `input` was given, in frames.
    #[must_use]
    pub fn video_delay(&self, input: InputId) -> u32 {
        self.video_delays.get(&input).copied().unwrap_or(0)
    }

    /// The source frame `input` presents at output `frame_number`: the frame
    /// `delay` intervals earlier, or `None` while the delay has not filled,
    /// which holds the input black during startup.
    #[must_use]
    pub fn presented_frame(&self, input: InputId, frame_number: u64) -> Option<u64> {
        frame_number.checked_sub(u64::from(self.video_delay(input)))
    }

    pub fn inputs(&self) -> impl ExactSizeIterator<Item = InputId> + '_ {
        self.sources.keys().copied()
    }

    /// Renders the switcher's desired program frame.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::MissingSource`] for either unavailable input, or
    /// a structured video error when generation or blending fails.
    pub fn render(
        &self,
        frame_number: u64,
        program: ProgramFrame,
    ) -> Result<ImageFrame, RenderError> {
        self.render_mapped(frame_number, program, |_, frame| Ok(frame))
    }

    /// Renders the program frame like [`Self::render`], passing each source
    /// picture through `map` (for example a per-input colour grade) before
    /// the transition mixes them.
    ///
    /// # Errors
    ///
    /// Returns every error of [`Self::render`] and any error of `map`.
    pub fn render_mapped(
        &self,
        frame_number: u64,
        program: ProgramFrame,
        map: impl Fn(InputId, ImageFrame) -> Result<ImageFrame, FrameError>,
    ) -> Result<ImageFrame, RenderError> {
        let primary = map(
            program.primary,
            self.render_source(program.primary, frame_number)?,
        )?;
        let secondary = match program
            .secondary
            .filter(|secondary| *secondary != program.primary)
        {
            Some(secondary) => Some(map(
                secondary,
                self.render_source(secondary, frame_number)?,
            )?),
            None => None,
        };
        mix_frames(&program, primary, secondary)
    }

    /// Renders a registered source for a frame number, applying its video
    /// delay (IN-022).
    ///
    /// Simulated pictures are static, so the delayed frame is normally the
    /// same pixels; the delay is visible during startup, when the input holds
    /// black until `frame_number` reaches it. Bars are rendered once per
    /// pipeline and copied afterwards.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::MissingSource`] when the input is not registered,
    /// or a structured video error when generation fails.
    pub fn render_source(
        &self,
        input: InputId,
        frame_number: u64,
    ) -> Result<ImageFrame, RenderError> {
        let source = self
            .sources
            .get(&input)
            .ok_or(RenderError::MissingSource { input })?;
        let Some(presented) = self.presented_frame(input, frame_number) else {
            // The delayed instant has not reached the source's first picture.
            return Ok(solid_color(
                self.width,
                self.height,
                Rgba8::new(0, 0, 0, u8::MAX),
            )?);
        };
        let _ = presented;
        Ok(match source.pattern() {
            SourcePattern::Bars => self.bars()?,
            SourcePattern::Solid(color) => solid_color(self.width, self.height, color)?,
        })
    }

    fn bars(&self) -> Result<ImageFrame, FrameError> {
        if let Some(bars) = self.bars.get() {
            return Ok(bars.clone());
        }
        let bars = render_test_signal_rgba8(SourcePattern::BARS, self.width, self.height)
            .map_err(crate::source::frame_error)?;
        Ok(self.bars.get_or_init(|| bars).clone())
    }
}

/// Mixes two already-rendered endpoint pictures according to `program`'s
/// transition. A renderer that resolves aliases, scenes, or feeds itself
/// draws both endpoints and lets this apply the switcher's mix.
///
/// # Errors
///
/// Returns [`RenderError::MissingTransitionKind`] when both endpoints are
/// present but the program names no transition, or the structured blend error
/// of a fade, alpha fade, or shape.
pub fn mix_frames(
    program: &ProgramFrame,
    primary: ImageFrame,
    secondary: Option<ImageFrame>,
) -> Result<ImageFrame, RenderError> {
    let Some(secondary) = program
        .secondary
        .filter(|secondary| *secondary != program.primary)
        .and(secondary)
    else {
        return Ok(primary);
    };
    match program.transition_kind {
        Some(TransitionKind::Fade | TransitionKind::AlphaFade) => Ok(crossfade(
            &primary,
            &secondary,
            program.mix_numerator,
            program.mix_denominator,
        )?),
        Some(kind) => match shape(kind) {
            Some(shape) => Ok(render_transition(
                shape,
                &primary,
                &secondary,
                program.mix_numerator,
                program.mix_denominator,
            )?),
            None => Err(RenderError::UnsupportedTransition(kind)),
        },
        None => Err(RenderError::MissingTransitionKind),
    }
}

/// The `fm-video` reference shape behind a geometric switcher mix; `None`
/// for Fade, `AlphaFade`, and Stinger, which are not shapes.
const fn shape(kind: TransitionKind) -> Option<TransitionShape> {
    Some(match kind {
        TransitionKind::Wipe => TransitionShape::Wipe,
        TransitionKind::VerticalWipe => TransitionShape::VerticalWipe,
        TransitionKind::Slide => TransitionShape::Slide,
        TransitionKind::VerticalSlide => TransitionShape::VerticalSlide,
        TransitionKind::Zoom => TransitionShape::Zoom,
        TransitionKind::Fly => TransitionShape::Fly,
        TransitionKind::FlyRotate => TransitionShape::FlyRotate,
        TransitionKind::Cube => TransitionShape::Cube,
        TransitionKind::CubeZoom => TransitionShape::CubeZoom,
        TransitionKind::CrossZoom => TransitionShape::CrossZoom,
        TransitionKind::Merge => TransitionShape::Merge,
        TransitionKind::Fade | TransitionKind::AlphaFade | TransitionKind::Stinger(_) => {
            return None;
        }
    })
}
