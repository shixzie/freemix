use fm_types::InputId;

use crate::StingerSlotId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    Fade,
    /// A horizontal wipe that reveals the target from left to right.
    Wipe,
    Slide,
    Zoom,
    AlphaFade,
    /// A vertical wipe that reveals the target from top to bottom.
    VerticalWipe,
    /// Program moves up while the target enters from the bottom.
    VerticalSlide,
    /// The target flies in from the right, growing to full size.
    Fly,
    /// Like [`Self::Fly`], turning half a turn as it lands.
    FlyRotate,
    /// A perspective cube turn: the target on the right face turns in.
    Cube,
    /// A cube turn that pulls back mid-way.
    CubeZoom,
    /// Program zooms in while crossfading to the target zooming out.
    CrossZoom,
    /// The target's two halves converge from the edges to the centre.
    Merge,
    Stinger(StingerSlotId),
}

impl TransitionKind {
    /// Every automatic mix transition kind other than Stinger, in operator
    /// order.
    pub const MIXES: [Self; 13] = [
        Self::Fade,
        Self::AlphaFade,
        Self::Wipe,
        Self::VerticalWipe,
        Self::Slide,
        Self::VerticalSlide,
        Self::Zoom,
        Self::Fly,
        Self::FlyRotate,
        Self::Cube,
        Self::CubeZoom,
        Self::CrossZoom,
        Self::Merge,
    ];
}

/// Normalized manual transition position expressed in basis points.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TBarPosition(u16);

impl TBarPosition {
    pub const MAX: u16 = 10_000;
    pub const START: Self = Self(0);
    pub const END: Self = Self(Self::MAX);

    #[must_use]
    pub const fn new(basis_points: u16) -> Option<Self> {
        if basis_points <= Self::MAX {
            Some(Self(basis_points))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn basis_points(self) -> u16 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TBarState {
    kind: TransitionKind,
    from: InputId,
    to: InputId,
    interval_start: TBarPosition,
    position: TBarPosition,
    /// The clip length a Stinger T-bar scrubs through; zero for every mix.
    clip_frames: u32,
}

impl TBarState {
    pub(crate) const fn new(kind: TransitionKind, from: InputId, to: InputId) -> Self {
        Self {
            kind,
            from,
            to,
            interval_start: TBarPosition::START,
            position: TBarPosition::START,
            clip_frames: 0,
        }
    }

    /// The same state scrubbing a Stinger clip of `clip_frames` frames.
    #[must_use]
    pub const fn with_clip_frames(mut self, clip_frames: u32) -> Self {
        self.clip_frames = clip_frames;
        self
    }

    /// The Stinger clip length this T-bar scrubs through (SW-004); zero for
    /// a mix.
    #[must_use]
    pub const fn clip_frames(self) -> u32 {
        self.clip_frames
    }

    /// The Stinger clip frame `position` shows: the position scaled to the
    /// clip, rounded down, and held on the last frame at the end of travel.
    #[must_use]
    pub fn clip_frame_at(self, position: TBarPosition) -> u32 {
        if self.clip_frames == 0 {
            return 0;
        }
        let frame = u64::from(position.basis_points()) * u64::from(self.clip_frames)
            / u64::from(TBarPosition::MAX);
        u32::try_from(frame)
            .unwrap_or(u32::MAX)
            .min(self.clip_frames - 1)
    }

    /// Reconstructs exact manual-transition state from a validated checkpoint.
    #[must_use]
    pub const fn restore(
        kind: TransitionKind,
        from: InputId,
        to: InputId,
        interval_start: TBarPosition,
        position: TBarPosition,
    ) -> Self {
        Self {
            kind,
            from,
            to,
            interval_start,
            position,
            clip_frames: 0,
        }
    }

    #[must_use]
    pub const fn kind(self) -> TransitionKind {
        self.kind
    }

    #[must_use]
    pub const fn from(self) -> InputId {
        self.from
    }

    #[must_use]
    pub const fn to(self) -> InputId {
        self.to
    }

    #[must_use]
    pub const fn position(self) -> TBarPosition {
        self.position
    }

    #[must_use]
    pub const fn interval_start(self) -> TBarPosition {
        self.interval_start
    }

    pub(crate) const fn set_position(&mut self, position: TBarPosition) {
        self.position = position;
    }

    pub(crate) const fn settle_frame(&mut self) {
        self.interval_start = self.position;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionState {
    kind: TransitionKind,
    from: InputId,
    to: InputId,
    duration_frames: u32,
    elapsed_frames: u32,
}

impl TransitionState {
    #[must_use]
    pub const fn new(
        kind: TransitionKind,
        from: InputId,
        to: InputId,
        duration_frames: u32,
    ) -> Self {
        Self {
            kind,
            from,
            to,
            duration_frames,
            elapsed_frames: 0,
        }
    }

    #[must_use]
    pub const fn kind(self) -> TransitionKind {
        self.kind
    }

    #[must_use]
    pub const fn from(self) -> InputId {
        self.from
    }

    #[must_use]
    pub const fn to(self) -> InputId {
        self.to
    }

    #[must_use]
    pub const fn duration_frames(self) -> u32 {
        self.duration_frames
    }

    #[must_use]
    pub const fn elapsed_frames(self) -> u32 {
        self.elapsed_frames
    }

    #[must_use]
    pub const fn mix_numerator(self) -> u32 {
        self.elapsed_frames
    }

    #[must_use]
    pub const fn mix_denominator(self) -> u32 {
        self.duration_frames
    }

    #[must_use]
    pub const fn mix_end_numerator(self) -> u32 {
        let next = self.elapsed_frames.saturating_add(1);
        if next < self.duration_frames {
            next
        } else {
            self.duration_frames
        }
    }

    pub(crate) const fn advance(&mut self) -> bool {
        self.elapsed_frames += 1;
        self.elapsed_frames >= self.duration_frames
    }
}
