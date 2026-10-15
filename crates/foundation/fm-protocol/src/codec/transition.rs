//! Transition kinds and favourite transition buttons (Protocol 2.36).
//!
//! A favourite is `<effect>:<duration_frames>`, where `<effect>` is `cut`,
//! an [`AutoTransitionKind`] token, or `stinger_<slot>`; the snapshot's four
//! favourites are joined by `;`, slot 1 first. Durations are canonical
//! decimals in `1..=`[`MAX_TRANSITION_FAVORITE_DURATION_FRAMES`] and slots
//! are `1..=`[`TRANSITION_FAVORITE_COUNT`].

use crate::{
    AutoTransitionKind, MAX_TRANSITION_FAVORITE_DURATION_FRAMES, TRANSITION_FAVORITE_COUNT,
    TransitionFavoriteEffect, TransitionFavoriteStatus,
};

use super::CodecError;
use super::value::parse_canonical_u64;

pub(super) fn decode_auto_transition_kind(
    value: String,
    field: &'static str,
) -> Result<AutoTransitionKind, CodecError> {
    AutoTransitionKind::from_token(&value).ok_or(CodecError::InvalidField { field, value })
}

pub(super) fn validate_favorite_slot(slot: u8, field: &'static str) -> Result<(), CodecError> {
    if slot >= 1 && usize::from(slot) <= TRANSITION_FAVORITE_COUNT {
        Ok(())
    } else {
        Err(CodecError::InvalidField {
            field,
            value: slot.to_string(),
        })
    }
}

pub(super) fn decode_favorite_slot(value: String, field: &'static str) -> Result<u8, CodecError> {
    parse_canonical_u64(&value)
        .and_then(|slot| u8::try_from(slot).ok())
        .filter(|slot| validate_favorite_slot(*slot, field).is_ok())
        .ok_or(CodecError::InvalidField { field, value })
}

pub(super) fn encode_favorite(
    favorite: TransitionFavoriteStatus,
    field: &'static str,
) -> Result<String, CodecError> {
    if favorite.duration_frames == 0
        || favorite.duration_frames > MAX_TRANSITION_FAVORITE_DURATION_FRAMES
    {
        return Err(CodecError::InvalidField {
            field,
            value: favorite.duration_frames.to_string(),
        });
    }
    Ok(format!(
        "{}:{}",
        favorite.effect.token(),
        favorite.duration_frames
    ))
}

pub(super) fn decode_favorite(
    value: &str,
    field: &'static str,
) -> Result<TransitionFavoriteStatus, CodecError> {
    let invalid = || CodecError::InvalidField {
        field,
        value: value.to_owned(),
    };
    let (effect, duration) = value.split_once(':').ok_or_else(invalid)?;
    let effect = TransitionFavoriteEffect::from_token(effect).ok_or_else(invalid)?;
    let duration_frames = parse_canonical_u64(duration)
        .and_then(|duration| u32::try_from(duration).ok())
        .filter(|duration| (1..=MAX_TRANSITION_FAVORITE_DURATION_FRAMES).contains(duration))
        .ok_or_else(invalid)?;
    Ok(TransitionFavoriteStatus {
        effect,
        duration_frames,
    })
}

pub(super) fn encode_favorites(
    favorites: &[TransitionFavoriteStatus; TRANSITION_FAVORITE_COUNT],
) -> Result<String, CodecError> {
    Ok(favorites
        .iter()
        .map(|favorite| encode_favorite(*favorite, "transition_favorites"))
        .collect::<Result<Vec<_>, _>>()?
        .join(";"))
}

pub(super) fn decode_favorites(
    value: &str,
) -> Result<[TransitionFavoriteStatus; TRANSITION_FAVORITE_COUNT], CodecError> {
    value
        .split(';')
        .map(|favorite| decode_favorite(favorite, "transition_favorites"))
        .collect::<Result<Vec<_>, _>>()?
        .try_into()
        .map_err(|_| CodecError::InvalidField {
            field: "transition_favorites",
            value: value.to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WireStingerSlotId;

    #[test]
    fn favourites_are_strict_effect_duration_pairs() {
        let stinger = TransitionFavoriteStatus {
            effect: TransitionFavoriteEffect::Stinger(WireStingerSlotId::new(8).unwrap()),
            duration_frames: 3_600,
        };
        assert_eq!(encode_favorite(stinger, "f").unwrap(), "stinger_8:3600");
        assert_eq!(decode_favorite("stinger_8:3600", "f").unwrap(), stinger);
        for invalid in [
            "stinger_9:10",
            "stinger_01:10",
            "stinger:10",
            "fade:0",
            "fade:3601",
            "fade:010",
            "fade",
            "Fade:10",
            "cut:1:2",
        ] {
            assert!(decode_favorite(invalid, "f").is_err(), "{invalid}");
        }
        assert!(decode_favorites("cut:1;fade:2;wipe:3").is_err());
        assert!(decode_favorites("cut:1;fade:2;wipe:3;merge:4;cube:5").is_err());
        assert_eq!(decode_favorite_slot("4".into(), "slot").unwrap(), 4);
        for slot in ["0", "5", "04", ""] {
            assert!(decode_favorite_slot(slot.into(), "slot").is_err(), "{slot}");
        }
    }
}
