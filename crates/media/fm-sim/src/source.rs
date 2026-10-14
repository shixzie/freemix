use fm_types::InputId;
use fm_video::{
    BarsColorimetry, BarsPattern, FrameError, GeneratedContent, Rgba8, TestSignalError,
    TestSignalSpec,
};

/// A static simulated picture. Neither pattern changes over time; frame
/// identity is carried by timing and frame numbers, never by pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourcePattern {
    /// Eight exact 75 % BT.709 colour bars ([`SourcePattern::BARS`]) as
    /// opaque full-range RGBA: white, yellow, cyan, green, magenta, red,
    /// blue and black at code 191/0.
    Bars,
    Solid(Rgba8),
}

impl SourcePattern {
    /// The test signal rendered for [`SourcePattern::Bars`].
    pub const BARS: TestSignalSpec = TestSignalSpec {
        pattern: BarsPattern::Bars75,
        colorimetry: BarsColorimetry::Bt709,
    };

    /// The generator content that renders this pattern.
    #[must_use]
    pub const fn generated_content(self) -> GeneratedContent {
        match self {
            Self::Bars => GeneratedContent::TestSignal(Self::BARS),
            Self::Solid(color) => GeneratedContent::Solid(color),
        }
    }
}

/// Folds a test-signal error into the frame error the simulated sources
/// report. The RGBA renderer only reports an invalid raster in practice.
pub(crate) const fn frame_error(error: TestSignalError) -> FrameError {
    match error {
        TestSignalError::Frame(error) => error,
        TestSignalError::PlaneTooLarge => FrameError::LayoutOverflow,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SimulatedSource {
    input: InputId,
    pattern: SourcePattern,
}

impl SimulatedSource {
    #[must_use]
    pub const fn new(input: InputId, pattern: SourcePattern) -> Self {
        Self { input, pattern }
    }

    #[must_use]
    pub const fn input(&self) -> InputId {
        self.input
    }

    #[must_use]
    pub const fn pattern(&self) -> SourcePattern {
        self.pattern
    }
}
