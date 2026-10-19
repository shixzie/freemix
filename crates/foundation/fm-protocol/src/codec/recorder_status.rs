//! `recorder_status` samples (Protocol 2.66): `recorders` is one
//! `<slot>:<state>:<elapsed_ms>:<failure>` entry per recorder slot in strict
//! slot order, `;`-separated, with `failure` escaped like a `stream_status`
//! failure and empty unless `state` is `failed`.

use crate::{
    CodecError, MAX_RECORDER_STATUS_SAMPLES, RecorderRealizedState, RecorderStatusSample,
    WireRecorderSlot,
};

use super::value::{escape_stream_failure, parse_canonical_u64, unescape};

fn invalid(value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field: "recorders",
        value: value.into(),
    }
}

/// Encodes the `recorders` field.
pub(super) fn encode_samples(samples: &[RecorderStatusSample]) -> Result<String, CodecError> {
    if samples.is_empty() {
        return Err(invalid("empty"));
    }
    if samples.len() > MAX_RECORDER_STATUS_SAMPLES {
        return Err(CodecError::TooManyItems("recorders"));
    }
    let mut previous = None;
    let entries = samples
        .iter()
        .map(|sample| {
            if previous.is_some_and(|slot| sample.recorder.get() <= slot) || !sample.is_valid() {
                return Err(invalid(sample.recorder.to_string()));
            }
            previous = Some(sample.recorder.get());
            Ok(format!(
                "{}:{}:{}:{}",
                sample.recorder,
                sample.realized.token(),
                sample.elapsed_ms,
                match &sample.failure {
                    Some(failure) => escape_stream_failure(failure)?,
                    None => String::new(),
                }
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(entries.join(";"))
}

/// Decodes the `recorders` field, refusing anything but the exact contract.
pub(super) fn decode_samples(value: &str) -> Result<Vec<RecorderStatusSample>, CodecError> {
    if value.is_empty() {
        return Err(invalid(value));
    }
    let entries: Vec<_> = value.split(';').collect();
    if entries.len() > MAX_RECORDER_STATUS_SAMPLES {
        return Err(CodecError::TooManyItems("recorders"));
    }
    let mut previous = None;
    entries
        .into_iter()
        .map(|entry| {
            let [slot, state, elapsed, failure] = entry.split(':').collect::<Vec<_>>()[..] else {
                return Err(invalid(entry));
            };
            let recorder = slot
                .parse::<u8>()
                .ok()
                .filter(|_| slot.len() == 1)
                .and_then(WireRecorderSlot::new)
                .ok_or_else(|| invalid(entry))?;
            if previous.is_some_and(|last| recorder.get() <= last) {
                return Err(invalid(entry));
            }
            previous = Some(recorder.get());
            let sample = RecorderStatusSample {
                recorder,
                realized: RecorderRealizedState::from_token(state).ok_or_else(|| invalid(entry))?,
                elapsed_ms: parse_canonical_u64(elapsed).ok_or_else(|| invalid(entry))?,
                failure: if failure.is_empty() {
                    None
                } else {
                    Some(unescape(failure)?)
                },
            };
            if sample.is_valid() {
                Ok(sample)
            } else {
                Err(invalid(entry))
            }
        })
        .collect()
}
