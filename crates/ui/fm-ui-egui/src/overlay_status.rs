use fm_protocol::{OverlayBorderPreset, OverlayPositionPreset};
use fm_types::InputId;
use fm_ui_model::OverlayStatus;

use crate::l10n::{Arg, Locale, Msg};

/// The overlay card's diagnostic line in the console language: the desired
/// state beside the daemon's last confirmation, with each source named by
/// `name`.
pub(super) fn format(
    l: Locale,
    channel: u8,
    desired: Option<&OverlayStatus>,
    confirmed: Option<&OverlayStatus>,
    name: impl Fn(InputId) -> String,
) -> String {
    l.format(
        Msg::OverlayDiagLine,
        &[
            ("channel", Arg::Plain(u64::from(channel))),
            ("desired", Arg::Str(&state(l, desired, &name))),
            ("confirmed", Arg::Str(&state(l, confirmed, &name))),
        ],
    )
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

fn state(l: Locale, status: Option<&OverlayStatus>, name: &impl Fn(InputId) -> String) -> String {
    let Some(status) = status else {
        return l.text(Msg::OverlayDiagUnavailable).to_owned();
    };
    l.format(
        Msg::OverlayDiagState,
        &[
            (
                "source",
                Arg::Str(
                    &status
                        .source
                        .map_or_else(|| l.text(Msg::None).to_owned(), name),
                ),
            ),
            (
                "active",
                Arg::Str(l.text(if status.active {
                    Msg::OverlayDiagActive
                } else {
                    Msg::OverlayDiagInactive
                })),
            ),
            ("opacity", Arg::Plain(u64::from(status.opacity))),
            ("queued", Arg::from(status.queued_sources.len())),
            (
                "position",
                Arg::Str(&l.text(position_label(status.position)).to_lowercase()),
            ),
            (
                "border",
                Arg::Str(&l.text(border_label(status.border)).to_lowercase()),
            ),
        ],
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

        let name = |input: InputId| format!("Cam {input}");
        assert_eq!(
            format(Locale::ENGLISH, 1, Some(&desired), Some(&confirmed), name),
            "O1 · desired: Cam 2, active, opacity 255, queue 1, tl, thin · \
             confirmed: none, inactive, opacity 0, queue 0, full, no border"
        );
        assert!(
            format(Locale::ENGLISH, 1, None, Some(&confirmed), name)
                .contains("desired: unavailable")
        );
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        assert_eq!(
            format(spanish, 1, Some(&desired), None, name),
            "O1 · pedido: Cam 2, activo, opacidad 255, cola 1, si, fino · confirmado: no disponible"
        );
    }
}
