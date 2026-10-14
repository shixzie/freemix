mod decoder;
mod encoder;
mod reader;

use crate::ProjectValidationError;

pub(crate) use decoder::decode;
pub(crate) use encoder::{encode, escape_string};
pub(crate) use reader::{Reader, Value};

#[derive(Debug)]
pub(crate) enum DecodeError {
    Syntax { offset: usize, message: String },
    Validation(ProjectValidationError),
}
