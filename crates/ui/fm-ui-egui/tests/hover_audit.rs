//! Hover-help audit (parity row RC-018): every interactive console, animation
//! timeline, and colour panel widget explains itself on hover, says why it is
//! disabled, names its shortcut, and is translated, in every shipped language.

use fm_ui_egui::hover_audit::{
    check, check_capture, check_color_panel, check_monitor_output, check_screen_capture,
    check_stills, check_system_audio, check_timeline,
};
use fm_ui_model::i18n::Language;

#[test]
fn every_shipped_language_is_audited() {
    // A new catalog language needs its own audit tests below.
    assert_eq!(Language::ALL, [Language::English, Language::Spanish]);
}

#[test]
fn every_interactive_widget_explains_itself_in_english() {
    check(Language::English);
}

#[test]
fn every_interactive_widget_explains_itself_in_spanish() {
    check(Language::Spanish);
}

#[test]
fn every_animation_timeline_widget_explains_itself_in_english() {
    check_timeline(Language::English);
}

#[test]
fn every_animation_timeline_widget_explains_itself_in_spanish() {
    check_timeline(Language::Spanish);
}

#[test]
fn every_color_panel_widget_explains_itself_in_english() {
    check_color_panel(Language::English);
}

#[test]
fn every_color_panel_widget_explains_itself_in_spanish() {
    check_color_panel(Language::Spanish);
}

#[test]
fn every_capture_form_widget_and_signal_lost_tile_explains_itself_in_english() {
    check_capture(Language::English);
}

#[test]
fn every_capture_form_widget_and_signal_lost_tile_explains_itself_in_spanish() {
    check_capture(Language::Spanish);
}

#[test]
fn every_system_audio_form_widget_explains_itself_in_english() {
    check_system_audio(Language::English);
}

#[test]
fn every_system_audio_form_widget_explains_itself_in_spanish() {
    check_system_audio(Language::Spanish);
}

#[test]
fn every_headphone_device_picker_widget_explains_itself_in_english() {
    check_monitor_output(Language::English);
}

#[test]
fn every_headphone_device_picker_widget_explains_itself_in_spanish() {
    check_monitor_output(Language::Spanish);
}

#[test]
fn every_image_and_slideshow_widget_explains_itself_in_english() {
    check_stills(Language::English);
}

#[test]
fn every_image_and_slideshow_widget_explains_itself_in_spanish() {
    check_stills(Language::Spanish);
}

#[test]
fn every_screen_capture_form_widget_explains_itself_in_english() {
    check_screen_capture(Language::English);
}

#[test]
fn every_screen_capture_form_widget_explains_itself_in_spanish() {
    check_screen_capture(Language::Spanish);
}
