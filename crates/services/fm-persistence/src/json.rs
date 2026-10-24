mod animation_preset;
mod audio_mixer;
mod capture;
mod cue;
mod decoder;
mod display_output;
mod document;
mod encoder;
mod grade;
mod input_lifecycle;
mod live_lan;
mod media_list;
mod media_playback;
mod network_stream;
mod reel;
mod replay;
mod scene_animation;
mod screen_capture;
mod screen_capture_tokens;
mod script;
mod shortcut;
mod still;
mod strip_dsp;
mod system_audio;
mod title_timeline;
mod transition_favorite;

use crate::ProjectValidationError;

pub(crate) use decoder::{
    Object, decode, parse_array, parse_hex_color, parse_optional, syntax, unknown_enum,
};
pub(crate) use encoder::{encode, escape_string, write_hex_color};
pub(crate) use fm_preset_file::json::{Reader, SyntaxError, Value};

#[derive(Debug)]
pub(crate) enum DecodeError {
    Syntax { offset: usize, message: String },
    Validation(ProjectValidationError),
}

impl From<SyntaxError> for DecodeError {
    fn from(SyntaxError { offset, message }: SyntaxError) -> Self {
        Self::Syntax { offset, message }
    }
}
