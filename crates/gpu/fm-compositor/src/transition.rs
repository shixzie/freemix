use core::fmt;

use fm_video::{BlendError, ImageFrame, TransitionShape, crossfade, render_transition};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    Cut,
    Fade,
    /// Independently crossfades every premultiplied RGBA channel.
    AlphaFade,
    /// Left-to-right wipe; see [`TransitionShape::Wipe`].
    Wipe,
    /// Top-to-bottom wipe; see [`TransitionShape::VerticalWipe`].
    VerticalWipe,
    /// Horizontal push; see [`TransitionShape::Slide`].
    Slide,
    /// Vertical push; see [`TransitionShape::VerticalSlide`].
    VerticalSlide,
    /// Centred nearest-neighbour grow; see [`TransitionShape::Zoom`].
    Zoom,
    /// Preview flies in from the right; see [`TransitionShape::Fly`].
    Fly,
    /// Fly with a half turn; see [`TransitionShape::FlyRotate`].
    FlyRotate,
    /// Perspective cube turn; see [`TransitionShape::Cube`].
    Cube,
    /// Cube turn that recedes and returns; see [`TransitionShape::CubeZoom`].
    CubeZoom,
    /// Zoom-through crossfade; see [`TransitionShape::CrossZoom`].
    CrossZoom,
    /// Preview halves meet in the centre; see [`TransitionShape::Merge`].
    Merge,
    Stinger,
}

impl TransitionKind {
    /// The `fm-video` reference shape behind a geometric kind, or `None` for
    /// Cut, Fade, `AlphaFade`, and Stinger.
    #[must_use]
    pub const fn shape(self) -> Option<TransitionShape> {
        match self {
            Self::Wipe => Some(TransitionShape::Wipe),
            Self::VerticalWipe => Some(TransitionShape::VerticalWipe),
            Self::Slide => Some(TransitionShape::Slide),
            Self::VerticalSlide => Some(TransitionShape::VerticalSlide),
            Self::Zoom => Some(TransitionShape::Zoom),
            Self::Fly => Some(TransitionShape::Fly),
            Self::FlyRotate => Some(TransitionShape::FlyRotate),
            Self::Cube => Some(TransitionShape::Cube),
            Self::CubeZoom => Some(TransitionShape::CubeZoom),
            Self::CrossZoom => Some(TransitionShape::CrossZoom),
            Self::Merge => Some(TransitionShape::Merge),
            Self::Cut | Self::Fade | Self::AlphaFade | Self::Stinger => None,
        }
    }
}

/// Validated transition work. Progress is retained as an exact rational value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionPlan {
    kind: TransitionKind,
    numerator: u32,
    denominator: u32,
}

impl TransitionPlan {
    /// Compiles any kind but Stinger (Cut, Fade, `AlphaFade`, or a geometric
    /// shape) at an exact rational progress value.
    ///
    /// # Errors
    /// Returns an error for a zero denominator, progress beyond the endpoint,
    /// or a transition not implemented in this phase.
    pub const fn compile(
        kind: TransitionKind,
        numerator: u32,
        denominator: u32,
    ) -> Result<Self, TransitionError> {
        if denominator == 0 {
            return Err(TransitionError::ZeroDenominator);
        }
        if numerator > denominator {
            return Err(TransitionError::ProgressOutOfRange {
                numerator,
                denominator,
            });
        }
        match kind {
            TransitionKind::Cut
            | TransitionKind::Fade
            | TransitionKind::AlphaFade
            | TransitionKind::Wipe
            | TransitionKind::VerticalWipe
            | TransitionKind::Slide
            | TransitionKind::VerticalSlide
            | TransitionKind::Zoom
            | TransitionKind::Fly
            | TransitionKind::FlyRotate
            | TransitionKind::Cube
            | TransitionKind::CubeZoom
            | TransitionKind::CrossZoom
            | TransitionKind::Merge => Ok(Self {
                kind,
                numerator,
                denominator,
            }),
            TransitionKind::Stinger => Err(TransitionError::UnsupportedKind(kind)),
        }
    }

    #[must_use]
    pub const fn kind(self) -> TransitionKind {
        self.kind
    }

    #[must_use]
    pub const fn numerator(self) -> u32 {
        self.numerator
    }

    #[must_use]
    pub const fn denominator(self) -> u32 {
        self.denominator
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionError {
    ZeroDenominator,
    ProgressOutOfRange { numerator: u32, denominator: u32 },
    UnsupportedKind(TransitionKind),
    Blend(BlendError),
}

impl fmt::Display for TransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDenominator => formatter.write_str("transition denominator must be nonzero"),
            Self::ProgressOutOfRange {
                numerator,
                denominator,
            } => write!(
                formatter,
                "transition progress {numerator}/{denominator} exceeds its endpoint"
            ),
            Self::UnsupportedKind(kind) => {
                write!(
                    formatter,
                    "transition {kind:?} is not supported in this phase"
                )
            }
            Self::Blend(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TransitionError {}

impl From<BlendError> for TransitionError {
    fn from(value: BlendError) -> Self {
        Self::Blend(value)
    }
}

/// Executes a Cut, exact integer Fade/AlphaFade, or geometric shape.
///
/// Cut is atomic and always returns `to`. Fade and `AlphaFade` return byte-identical endpoint
/// clones at zero and full progress through `fm-video`'s reference crossfade. `AlphaFade`
/// intentionally interpolates alpha together with the color channels for transparent content.
/// Every other kind delegates to [`fm_video::render_transition`] with its
/// [`TransitionKind::shape`], which documents the exact geometry and also returns
/// byte-identical endpoints: Wipe, `VerticalWipe`, Slide, `VerticalSlide`, and Merge copy whole
/// texels at floored integer extents, Zoom samples nearest-neighbour, and Fly, `FlyRotate`,
/// Cube, `CubeZoom`, and `CrossZoom` sample bilinearly at 1/256-texel precision.
///
/// # Errors
/// Returns an error if the transition inputs have incompatible layouts.
pub fn execute_transition(
    plan: TransitionPlan,
    from: &ImageFrame,
    to: &ImageFrame,
) -> Result<ImageFrame, TransitionError> {
    match plan.kind {
        TransitionKind::Cut => Ok(to.clone()),
        TransitionKind::Fade | TransitionKind::AlphaFade => {
            Ok(crossfade(from, to, plan.numerator, plan.denominator)?)
        }
        kind => {
            let shape = kind.shape().ok_or(TransitionError::UnsupportedKind(kind))?;
            Ok(render_transition(
                shape,
                from,
                to,
                plan.numerator,
                plan.denominator,
            )?)
        }
    }
}
