//! The transition family on the wire (SW-002, SW-005, Protocol 2.36):
//! automatic mixes taken by kind, overlay channel transitions, and the
//! project's favourite transition buttons.

use fm_engine::{Engine, EngineCommand};
use fm_model::{StingerSlotNumber, TransitionEffect, TransitionFavorite};
use fm_protocol::{
    AutoTransitionKind, EventPayload, OverlayTransitionKind as ProtocolOverlayTransition,
    TRANSITION_FAVORITE_COUNT, TransitionFavoriteEffect, TransitionFavoriteStatus,
    WireStingerSlotId,
};
use fm_switcher::{OverlayTransitionKind as SwitcherOverlayTransition, TransitionKind};

/// The wire form of a switcher overlay transition kind.
#[must_use]
pub const fn wire_overlay_transition(
    transition: SwitcherOverlayTransition,
) -> ProtocolOverlayTransition {
    match transition {
        SwitcherOverlayTransition::Cut => ProtocolOverlayTransition::Cut,
        SwitcherOverlayTransition::Fade => ProtocolOverlayTransition::Fade,
        SwitcherOverlayTransition::Wipe => ProtocolOverlayTransition::Wipe,
        SwitcherOverlayTransition::VerticalWipe => ProtocolOverlayTransition::VerticalWipe,
        SwitcherOverlayTransition::Slide => ProtocolOverlayTransition::Slide,
        SwitcherOverlayTransition::VerticalSlide => ProtocolOverlayTransition::VerticalSlide,
        SwitcherOverlayTransition::Zoom => ProtocolOverlayTransition::Zoom,
        SwitcherOverlayTransition::Fly => ProtocolOverlayTransition::Fly,
        SwitcherOverlayTransition::FlyRotate => ProtocolOverlayTransition::FlyRotate,
        SwitcherOverlayTransition::Cube => ProtocolOverlayTransition::Cube,
        SwitcherOverlayTransition::CubeZoom => ProtocolOverlayTransition::CubeZoom,
        SwitcherOverlayTransition::CrossZoom => ProtocolOverlayTransition::CrossZoom,
        SwitcherOverlayTransition::Merge => ProtocolOverlayTransition::Merge,
    }
}

/// The switcher form of a wire overlay transition kind.
#[must_use]
pub const fn domain_overlay_transition(
    transition: ProtocolOverlayTransition,
) -> SwitcherOverlayTransition {
    match transition {
        ProtocolOverlayTransition::Cut => SwitcherOverlayTransition::Cut,
        ProtocolOverlayTransition::Fade => SwitcherOverlayTransition::Fade,
        ProtocolOverlayTransition::Wipe => SwitcherOverlayTransition::Wipe,
        ProtocolOverlayTransition::VerticalWipe => SwitcherOverlayTransition::VerticalWipe,
        ProtocolOverlayTransition::Slide => SwitcherOverlayTransition::Slide,
        ProtocolOverlayTransition::VerticalSlide => SwitcherOverlayTransition::VerticalSlide,
        ProtocolOverlayTransition::Zoom => SwitcherOverlayTransition::Zoom,
        ProtocolOverlayTransition::Fly => SwitcherOverlayTransition::Fly,
        ProtocolOverlayTransition::FlyRotate => SwitcherOverlayTransition::FlyRotate,
        ProtocolOverlayTransition::Cube => SwitcherOverlayTransition::Cube,
        ProtocolOverlayTransition::CubeZoom => SwitcherOverlayTransition::CubeZoom,
        ProtocolOverlayTransition::CrossZoom => SwitcherOverlayTransition::CrossZoom,
        ProtocolOverlayTransition::Merge => SwitcherOverlayTransition::Merge,
    }
}

/// The switcher mix behind a wire automatic transition kind.
#[must_use]
pub const fn domain_transition_kind(kind: AutoTransitionKind) -> TransitionKind {
    match kind {
        AutoTransitionKind::Fade => TransitionKind::Fade,
        AutoTransitionKind::AlphaFade => TransitionKind::AlphaFade,
        AutoTransitionKind::Wipe => TransitionKind::Wipe,
        AutoTransitionKind::VerticalWipe => TransitionKind::VerticalWipe,
        AutoTransitionKind::Slide => TransitionKind::Slide,
        AutoTransitionKind::VerticalSlide => TransitionKind::VerticalSlide,
        AutoTransitionKind::Zoom => TransitionKind::Zoom,
        AutoTransitionKind::Fly => TransitionKind::Fly,
        AutoTransitionKind::FlyRotate => TransitionKind::FlyRotate,
        AutoTransitionKind::Cube => TransitionKind::Cube,
        AutoTransitionKind::CubeZoom => TransitionKind::CubeZoom,
        AutoTransitionKind::CrossZoom => TransitionKind::CrossZoom,
        AutoTransitionKind::Merge => TransitionKind::Merge,
    }
}

const fn wire_effect(effect: TransitionEffect) -> TransitionFavoriteEffect {
    let kind = match effect {
        TransitionEffect::Cut => return TransitionFavoriteEffect::Cut,
        TransitionEffect::Stinger(slot) => {
            let Some(slot) = WireStingerSlotId::new(slot.number()) else {
                unreachable!()
            };
            return TransitionFavoriteEffect::Stinger(slot);
        }
        TransitionEffect::Fade => AutoTransitionKind::Fade,
        TransitionEffect::AlphaFade => AutoTransitionKind::AlphaFade,
        TransitionEffect::Wipe => AutoTransitionKind::Wipe,
        TransitionEffect::VerticalWipe => AutoTransitionKind::VerticalWipe,
        TransitionEffect::Slide => AutoTransitionKind::Slide,
        TransitionEffect::VerticalSlide => AutoTransitionKind::VerticalSlide,
        TransitionEffect::Zoom => AutoTransitionKind::Zoom,
        TransitionEffect::Fly => AutoTransitionKind::Fly,
        TransitionEffect::FlyRotate => AutoTransitionKind::FlyRotate,
        TransitionEffect::Cube => AutoTransitionKind::Cube,
        TransitionEffect::CubeZoom => AutoTransitionKind::CubeZoom,
        TransitionEffect::CrossZoom => AutoTransitionKind::CrossZoom,
        TransitionEffect::Merge => AutoTransitionKind::Merge,
    };
    TransitionFavoriteEffect::Mix(kind)
}

/// The project form of a wire favourite effect.
#[must_use]
pub const fn domain_effect(effect: TransitionFavoriteEffect) -> TransitionEffect {
    match effect {
        TransitionFavoriteEffect::Cut => TransitionEffect::Cut,
        TransitionFavoriteEffect::Stinger(slot) => {
            let Some(slot) = StingerSlotNumber::new(slot.number()) else {
                unreachable!()
            };
            TransitionEffect::Stinger(slot)
        }
        TransitionFavoriteEffect::Mix(kind) => match kind {
            AutoTransitionKind::Fade => TransitionEffect::Fade,
            AutoTransitionKind::AlphaFade => TransitionEffect::AlphaFade,
            AutoTransitionKind::Wipe => TransitionEffect::Wipe,
            AutoTransitionKind::VerticalWipe => TransitionEffect::VerticalWipe,
            AutoTransitionKind::Slide => TransitionEffect::Slide,
            AutoTransitionKind::VerticalSlide => TransitionEffect::VerticalSlide,
            AutoTransitionKind::Zoom => TransitionEffect::Zoom,
            AutoTransitionKind::Fly => TransitionEffect::Fly,
            AutoTransitionKind::FlyRotate => TransitionEffect::FlyRotate,
            AutoTransitionKind::Cube => TransitionEffect::Cube,
            AutoTransitionKind::CubeZoom => TransitionEffect::CubeZoom,
            AutoTransitionKind::CrossZoom => TransitionEffect::CrossZoom,
            AutoTransitionKind::Merge => TransitionEffect::Merge,
        },
    }
}

/// The wire form of a project favourite.
#[must_use]
pub const fn wire_favorite(favorite: TransitionFavorite) -> TransitionFavoriteStatus {
    TransitionFavoriteStatus {
        effect: wire_effect(favorite.effect()),
        duration_frames: favorite.duration_frames(),
    }
}

/// The engine command assigning `slot`. The codec bounds the duration, so
/// the favourite is valid.
pub(crate) fn set_favorite_command(slot: u8, favorite: TransitionFavoriteStatus) -> EngineCommand {
    EngineCommand::SetTransitionFavorite {
        slot,
        favorite: TransitionFavorite::new(domain_effect(favorite.effect), favorite.duration_frames)
            .expect("the codec bounds favourite durations"),
    }
}

/// The favourites at the engine's current revision, slot 1 first.
pub(crate) fn wire_favorites(
    engine: &Engine,
) -> [TransitionFavoriteStatus; TRANSITION_FAVORITE_COUNT] {
    engine
        .show()
        .transition_favorites()
        .slots()
        .map(wire_favorite)
}

/// The `transition_favorite_changed` event for an accepted assignment.
pub(crate) fn favorite_changed(engine: &Engine, slot: u8) -> EventPayload {
    EventPayload::TransitionFavoriteChanged {
        slot,
        favorite: wire_favorite(
            engine
                .show()
                .transition_favorites()
                .get(slot)
                .expect("an accepted assignment names a favourite slot"),
        ),
    }
}
