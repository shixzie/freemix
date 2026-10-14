use fm_protocol::{OverlayBorderPreset, OverlayPositionPreset};
use fm_ui_model::OverlayStatus;

use crate::l10n::Msg;

pub(super) fn format(
    channel: u8,
    desired: Option<&OverlayStatus>,
    confirmed: Option<&OverlayStatus>,
) -> String {
    let desired = state(desired);
    format!("O{channel} · D: {desired} · C: {}", state(confirmed))
}

/// The position preset key label.
pub(super) const fn position_label(position: OverlayPositionPreset) -> Msg {
    match position {
        OverlayPositionPreset::FullFrame => Msg::PositionFull,
        OverlayPositionPreset::TopLeft => Msg::PositionTopLeft,
        OverlayPositionPreset::TopRight => Msg::PositionTopRight,
        OverlayPositionPreset::BottomLeft => Msg::PositionBottomLeft,
        OverlayPositionPreset::BottomRight => Msg::PositionBottomRight,
    }
}

/// The border preset key label.
pub(super) const fn border_label(border: OverlayBorderPreset) -> Msg {
    match border {
        OverlayBorderPreset::None => Msg::BorderNone,
        OverlayBorderPreset::ThinWhite => Msg::BorderThin,
        OverlayBorderPreset::ThickWhite => Msg::BorderThick,
    }
}

fn state(status: Option<&OverlayStatus>) -> String {
    let Some(status) = status else {
        return "unavailable".to_owned();
    };
    // The diagnostic line keeps the protocol's own English vocabulary.
    let position = position_label(status.position).en();
    let position = if position == "FULL" { "full" } else { position };
    format!(
        "src {}, {}, op {}, q {}, {}, {}",
        status
            .source
            .map_or_else(|| "none".to_owned(), |source| source.to_string()),
        if status.active { "active" } else { "inactive" },
        status.opacity,
        status.queued_sources.len(),
        position,
        match status.border {
            OverlayBorderPreset::None => "none",
            OverlayBorderPreset::ThinWhite => "thin",
            OverlayBorderPreset::ThickWhite => "thick",
        },
    )
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_protocol::{OverlayBorderPreset, OverlayPositionPreset, OverlayTransitionKind};
    use fm_types::InputId;

    use super::*;

    fn overlay(source: Option<u128>, active: bool, opacity: u8) -> OverlayStatus {
        OverlayStatus {
            channel: 1,
            source: source.map(|source| InputId::new(NonZeroU128::new(source).unwrap())),
            active,
            opacity,
            transition: OverlayTransitionKind::Cut,
            duration_frames: 30,
            position: if active {
                OverlayPositionPreset::TopLeft
            } else {
                OverlayPositionPreset::FullFrame
            },
            border: if active {
                OverlayBorderPreset::ThinWhite
            } else {
                OverlayBorderPreset::None
            },
            queued_sources: if active {
                vec![InputId::new(NonZeroU128::new(3).unwrap())]
            } else {
                Vec::new()
            },
            included_outputs: Vec::new(),
        }
    }

    #[test]
    fn desired_take_is_distinct_from_last_daemon_confirmation() {
        let desired = overlay(Some(2), true, u8::MAX);
        let confirmed = overlay(None, false, 0);

        assert_eq!(
            format(1, Some(&desired), Some(&confirmed)),
            "O1 · D: src 2, active, op 255, q 1, TL, thin · C: src none, inactive, op 0, q 0, full, none"
        );
        assert!(format(1, None, Some(&confirmed)).contains("D: unavailable"));
    }
}
