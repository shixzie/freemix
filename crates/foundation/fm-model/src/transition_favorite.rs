//! Operator-assignable favourite transition buttons (SW-002, schema 35).
//!
//! A project keeps exactly [`TRANSITION_FAVORITE_COUNT`] favourite slots.
//! Each holds one transition effect (Cut, a mix, or a Stinger slot) and its
//! own duration, so the operator takes a favourite with one button or key
//! regardless of the shared transition duration. A favourite is valid by
//! construction: its duration lies in
//! `1..=`[`MAX_TRANSITION_FAVORITE_DURATION_FRAMES`].

use core::fmt;

use crate::StingerSlotNumber;

/// Number of favourite transition buttons in every project.
pub const TRANSITION_FAVORITE_COUNT: usize = 4;
/// Longest favourite duration, matching every automatic transition.
pub const MAX_TRANSITION_FAVORITE_DURATION_FRAMES: u32 = 3_600;
/// Duration of the default favourites.
pub const DEFAULT_TRANSITION_FAVORITE_DURATION_FRAMES: u32 = 30;

/// The effect a favourite takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionEffect {
    Cut,
    Fade,
    AlphaFade,
    Wipe,
    VerticalWipe,
    Slide,
    VerticalSlide,
    Zoom,
    Fly,
    FlyRotate,
    Cube,
    CubeZoom,
    CrossZoom,
    Merge,
    Stinger(StingerSlotNumber),
}

impl TransitionEffect {
    /// Cut and every automatic mix, in operator order (Stingers excluded).
    pub const BUILT_IN: [Self; 14] = [
        Self::Cut,
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

    /// The stable lower-snake-case token of a built-in effect, or `None`
    /// for a Stinger (which names its slot separately).
    #[must_use]
    pub const fn token(self) -> Option<&'static str> {
        Some(match self {
            Self::Cut => "cut",
            Self::Fade => "fade",
            Self::AlphaFade => "alpha_fade",
            Self::Wipe => "wipe",
            Self::VerticalWipe => "vertical_wipe",
            Self::Slide => "slide",
            Self::VerticalSlide => "vertical_slide",
            Self::Zoom => "zoom",
            Self::Fly => "fly",
            Self::FlyRotate => "fly_rotate",
            Self::Cube => "cube",
            Self::CubeZoom => "cube_zoom",
            Self::CrossZoom => "cross_zoom",
            Self::Merge => "merge",
            Self::Stinger(_) => return None,
        })
    }

    /// The built-in effect named by `token`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::BUILT_IN
            .into_iter()
            .find(|effect| effect.token() == Some(token))
    }
}

/// One favourite: an effect and its duration in frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionFavorite {
    effect: TransitionEffect,
    duration_frames: u32,
}

/// A favourite duration outside `1..=`[`MAX_TRANSITION_FAVORITE_DURATION_FRAMES`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionFavoriteDurationError {
    pub duration_frames: u32,
}

impl fmt::Display for TransitionFavoriteDurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "favourite transition duration {} must be 1..={MAX_TRANSITION_FAVORITE_DURATION_FRAMES} frames",
            self.duration_frames
        )
    }
}

impl std::error::Error for TransitionFavoriteDurationError {}

impl TransitionFavorite {
    /// A favourite taking `effect` over `duration_frames`.
    ///
    /// # Errors
    ///
    /// Refuses a duration outside
    /// `1..=`[`MAX_TRANSITION_FAVORITE_DURATION_FRAMES`].
    pub const fn new(
        effect: TransitionEffect,
        duration_frames: u32,
    ) -> Result<Self, TransitionFavoriteDurationError> {
        if duration_frames == 0 || duration_frames > MAX_TRANSITION_FAVORITE_DURATION_FRAMES {
            return Err(TransitionFavoriteDurationError { duration_frames });
        }
        Ok(Self {
            effect,
            duration_frames,
        })
    }

    #[must_use]
    pub const fn effect(self) -> TransitionEffect {
        self.effect
    }

    #[must_use]
    pub const fn duration_frames(self) -> u32 {
        self.duration_frames
    }
}

/// The project's favourite transition buttons, slot 1 first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionFavorites([TransitionFavorite; TRANSITION_FAVORITE_COUNT]);

impl Default for TransitionFavorites {
    /// Fade, Wipe, Fly, and Cube, each over
    /// [`DEFAULT_TRANSITION_FAVORITE_DURATION_FRAMES`].
    fn default() -> Self {
        let favorite = |effect| TransitionFavorite {
            effect,
            duration_frames: DEFAULT_TRANSITION_FAVORITE_DURATION_FRAMES,
        };
        Self([
            favorite(TransitionEffect::Fade),
            favorite(TransitionEffect::Wipe),
            favorite(TransitionEffect::Fly),
            favorite(TransitionEffect::Cube),
        ])
    }
}

impl TransitionFavorites {
    #[must_use]
    pub const fn new(slots: [TransitionFavorite; TRANSITION_FAVORITE_COUNT]) -> Self {
        Self(slots)
    }

    /// Every slot, slot 1 first.
    #[must_use]
    pub const fn slots(&self) -> &[TransitionFavorite; TRANSITION_FAVORITE_COUNT] {
        &self.0
    }

    /// The favourite in one-based `slot`, or `None` outside
    /// `1..=`[`TRANSITION_FAVORITE_COUNT`].
    #[must_use]
    pub fn get(&self, slot: u8) -> Option<TransitionFavorite> {
        let index = usize::from(slot.checked_sub(1)?);
        self.0.get(index).copied()
    }

    /// Replaces the favourite in one-based `slot`, returning the previous
    /// one, or `None` (and no change) outside `1..=`[`TRANSITION_FAVORITE_COUNT`].
    pub fn set(&mut self, slot: u8, favorite: TransitionFavorite) -> Option<TransitionFavorite> {
        let index = usize::from(slot.checked_sub(1)?);
        let current = self.0.get_mut(index)?;
        Some(core::mem::replace(current, favorite))
    }
}
