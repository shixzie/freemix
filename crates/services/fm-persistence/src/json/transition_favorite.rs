//! The favourite transition buttons in `project.json` (schema 35, SW-002).
//!
//! ```text
//! "transition_favorites": [
//!   {"effect": "fade", "duration_frames": 30},
//!   {"effect": "wipe", "duration_frames": 30},
//!   {"effect": "stinger", "slot": 2, "duration_frames": 45},
//!   {"effect": "cube", "duration_frames": 30}
//! ]
//! ```
//!
//! Exactly [`fm_model::TRANSITION_FAVORITE_COUNT`] entries, slot 1 first.
//! `effect` is a [`fm_model::TransitionEffect`] token or `"stinger"`, which
//! alone carries a `slot` (1 through 8). Durations are
//! `1..=`[`fm_model::MAX_TRANSITION_FAVORITE_DURATION_FRAMES`].

use core::fmt::Write as _;

use fm_model::{
    StingerSlotNumber, TRANSITION_FAVORITE_COUNT, TransitionEffect, TransitionFavorite,
    TransitionFavorites,
};

use super::{DecodeError, Object, parse_array, syntax};

/// Writes `,\n    "transition_favorites": [...]` after the animation presets.
pub(crate) fn write_transition_favorites(output: &mut String, favorites: &TransitionFavorites) {
    output.push_str(",\n    \"transition_favorites\": [");
    for (index, favorite) in favorites.slots().iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        match favorite.effect() {
            TransitionEffect::Stinger(slot) => write!(
                output,
                "{{\"effect\": \"stinger\", \"slot\": {}, ",
                slot.number()
            ),
            effect => write!(
                output,
                "{{\"effect\": \"{}\", ",
                effect.token().expect("built-in effects have tokens")
            ),
        }
        .expect("writing to a string cannot fail");
        write!(
            output,
            "\"duration_frames\": {}}}",
            favorite.duration_frames()
        )
        .expect("writing to a string cannot fail");
    }
    output.push_str("\n    ]");
}

/// Parses `"transition_favorites"` from the project object.
pub(crate) fn parse_transition_favorites(
    object: &mut Object,
) -> Result<TransitionFavorites, DecodeError> {
    let favorites = parse_array(
        object.take("transition_favorites")?,
        "transition_favorites",
        |value| {
            let mut object = Object::new(value, "transition favourite")?;
            let effect = match object.string("effect")?.as_str() {
                "stinger" => {
                    let slot = object.u8("slot")?;
                    TransitionEffect::Stinger(
                        StingerSlotNumber::new(slot)
                            .ok_or_else(|| syntax("favourite stinger slot must be 1 through 8"))?,
                    )
                }
                token => TransitionEffect::from_token(token).ok_or_else(|| {
                    syntax(format!("unknown favourite transition effect `{token}`"))
                })?,
            };
            let duration_frames = object.u32("duration_frames")?;
            object.finish()?;
            TransitionFavorite::new(effect, duration_frames)
                .map_err(|error| syntax(error.to_string()))
        },
    )?;
    let slots: [TransitionFavorite; TRANSITION_FAVORITE_COUNT] =
        favorites.try_into().map_err(|_| {
            syntax(format!(
                "transition_favorites must hold exactly {TRANSITION_FAVORITE_COUNT} favourites"
            ))
        })?;
    Ok(TransitionFavorites::new(slots))
}
