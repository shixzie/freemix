//! The console's ambient locale and direction-aware layout helpers.
//!
//! [`crate::StudioShell`] owns the operator's [`Locale`] and stores it in
//! the egui context at the start of every frame, so drawing code reads it
//! with [`locale`] instead of threading it through every function, the same
//! way it reads the style. Pure helpers that produce text take a `Locale`
//! argument instead, so tests can pin a language.
//!
//! Layout code places content as "leading" (where reading starts) and
//! "trailing" (where it ends) through [`leading_layout`] and
//! [`trailing_layout`], so a right-to-left catalog mirrors bars and headers
//! without touching their call sites.

use egui::{Align, Context, Id, Layout};
pub(crate) use fm_ui_model::i18n::{Arg, Locale, Msg, TextDirection};

fn locale_id() -> Id {
    Id::new("fm-ui-egui-locale")
}

/// The locale the shell stored for this frame; English outside a shell.
pub(crate) fn locale(context: &Context) -> Locale {
    context
        .data(|data| data.get_temp::<Locale>(locale_id()))
        .unwrap_or_default()
}

/// Stores the frame's locale for [`locale`].
pub(crate) fn set_locale(context: &Context, locale: Locale) {
    context.data_mut(|data| data.insert_temp(locale_id(), locale));
}

/// `Unavailable: {reason}`, with a known English reason translated.
pub(crate) fn unavailable(locale: Locale, reason: &str) -> String {
    locale.format(Msg::Unavailable, &[("reason", Arg::Str(locale.tr(reason)))])
}

/// `{action}` above `Unavailable: {reason}`, for a disabled control's
/// tooltip.
pub(crate) fn disabled_text(locale: Locale, action: &str, reason: &str) -> String {
    format!("{action}\n{}", unavailable(locale, reason))
}

/// `{action}  [{key}]`: an action with its keyboard shortcut, for tooltips.
pub(crate) fn with_key(locale: Locale, action: &str, key: &str) -> String {
    locale.format(
        Msg::KeyHint,
        &[("action", Arg::Str(action)), ("key", Arg::Str(key))],
    )
}

/// `{frames} fr`, a frame count with the locale's digit grouping.
pub(crate) fn frames(locale: Locale, frames: u32) -> String {
    locale.format(Msg::FramesSuffix, &[("frames", Arg::from(frames))])
}

/// A row that starts where reading starts.
pub(crate) fn leading_layout(locale: Locale) -> Layout {
    match locale.direction() {
        TextDirection::LeftToRight => Layout::left_to_right(Align::Center),
        TextDirection::RightToLeft => Layout::right_to_left(Align::Center),
    }
    .with_cross_align(Align::Center)
}

/// [`egui::Ui::horizontal_centered`] in reading order: a vertically
/// centered row that starts on the leading side.
pub(crate) fn leading_row<R>(
    ui: &mut egui::Ui,
    locale: Locale,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let size = ui.available_size_before_wrap();
    ui.allocate_ui_with_layout(size, leading_layout(locale), add_contents)
}

/// A row that packs from where reading ends, for trailing clusters such as
/// the top bar's clock and on-air badges.
pub(crate) fn trailing_layout(locale: Locale) -> Layout {
    match locale.direction() {
        TextDirection::LeftToRight => Layout::right_to_left(Align::Center),
        TextDirection::RightToLeft => Layout::left_to_right(Align::Center),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_ui_model::i18n::Language;

    #[test]
    fn the_context_carries_the_frame_locale_and_defaults_to_english() {
        let context = Context::default();
        assert_eq!(locale(&context), Locale::ENGLISH);
        set_locale(&context, Locale::new(Language::Spanish));
        assert_eq!(locale(&context).language, Language::Spanish);
    }

    #[test]
    fn unavailable_reasons_are_translated_when_known() {
        let spanish = Locale::new(Language::Spanish);
        assert_eq!(
            unavailable(Locale::ENGLISH, Msg::ReasonNotReady.en()),
            "Unavailable: the control connection is not ready yet"
        );
        assert_eq!(
            unavailable(spanish, Msg::ReasonNotReady.en()),
            "No disponible: la conexión de control aún no está lista"
        );
        // Text outside the catalog (a daemon message) passes through.
        assert_eq!(
            unavailable(spanish, "disk full"),
            "No disponible: disk full"
        );
    }

    #[test]
    fn shortcut_hints_name_the_key_in_brackets() {
        let spanish = Locale::new(Language::Spanish);
        assert_eq!(with_key(Locale::ENGLISH, "Cut", "C"), "Cut  [C]");
        assert_eq!(
            with_key(spanish, "Añadir", spanish.text(Msg::KeyEnter)),
            "Añadir  [Intro]"
        );
        assert_eq!(frames(Locale::ENGLISH, 30), "30 fr");
    }

    #[test]
    fn trailing_clusters_follow_the_reading_direction() {
        let layout = trailing_layout(Locale::ENGLISH);
        assert!(layout.prefer_right_to_left());
        assert!(!leading_layout(Locale::ENGLISH).prefer_right_to_left());
    }
}
