//! `recorder_status` samples (Protocol 2.66): `recorders` is one
//! `<slot>:<state>:<elapsed_ms>:<failure>:<free bytes or ->:<remaining or ->`
//! entry per recorder slot in strict slot order, `;`-separated, with
//! `failure` an [`crate::OutputFailure`] token (Protocol 2.67), empty unless
//! `state` is `failed`, the free bytes on the slot's output filesystem when
//! the daemon samples them (Protocol 2.140), and the estimated seconds of
//! recording left at the configured bitrate (Protocol 2.141).

use crate::{
    CodecError, MAX_RECORDER_STATUS_SAMPLES, OutputFailure, RecorderRealizedState,
    RecorderStatusSample, WireRecorderSlot,
};

use super::value::parse_canonical_u64;

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
                "{}:{}:{}:{}:{}:{}",
                sample.recorder,
                sample.realized.token(),
                sample.elapsed_ms,
                sample
                    .failure
                    .map_or_else(String::new, |failure| failure.to_string()),
                sample
                    .free_bytes
                    .map_or_else(|| "-".to_owned(), |bytes| bytes.to_string()),
                sample
                    .remaining_seconds
                    .map_or_else(|| "-".to_owned(), |seconds| seconds.to_string())
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
            let [slot, state, elapsed, failure, free, remaining] =
                entry.split(':').collect::<Vec<_>>()[..]
            else {
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
                    Some(OutputFailure::parse(failure).ok_or_else(|| invalid(entry))?)
                },
                free_bytes: if free == "-" {
                    None
                } else {
                    Some(parse_canonical_u64(free).ok_or_else(|| invalid(entry))?)
                },
                remaining_seconds: if remaining == "-" {
                    None
                } else {
                    Some(parse_canonical_u64(remaining).ok_or_else(|| invalid(entry))?)
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
