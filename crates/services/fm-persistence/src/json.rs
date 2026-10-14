mod decoder;
mod encoder;
mod reader;

use crate::ProjectValidationError;

pub(crate) use decoder::{
    Object, decode, parse_array, parse_hex_color, parse_optional, syntax, unknown_enum,
};
pub(crate) use encoder::{encode, escape_string, write_hex_color};
pub(crate) use reader::{Reader, Value};

#[derive(Debug)]
pub(crate) enum DecodeError {
    Syntax { offset: usize, message: String },
    Validation(ProjectValidationError),
}
