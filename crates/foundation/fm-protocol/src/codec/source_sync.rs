//! `source_sync_status` samples (Protocol 2.76): `sources` is one
//! `<input>:<kind>:<state>:<input_hz>:<output_hz>:<drift_mppm>:<correction_mppm>:<buffer_us>:<target_us>:<resyncs>:<underruns>:<overruns>:<picture_drops>:<picture_repeats>:<restarts>`
//! entry per live source in strictly increasing input order, `;`-separated,
//! and empty when no live source runs. `diagnostics_response` carries the
//! same list in its `source_sync` field.

use crate::{
    CodecError, MAX_SOURCE_SYNC_SAMPLES, SourceClockState, SourceSyncKind, SourceSyncSample,
};

use super::value::{parse_canonical_input, parse_canonical_u64};

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

/// Parses a canonical signed decimal: `0`, or an optional `-` and digits
/// without a leading zero, exactly as `i64::to_string` writes it.
fn parse_canonical_i64(value: &str) -> Option<i64> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty()
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || (digits.starts_with('0') && value != "0")
    {
        return None;
    }
    value.parse().ok()
}

/// Encodes a sample list into `field`.
pub(super) fn encode_samples(
    field: &'static str,
    samples: &[SourceSyncSample],
) -> Result<String, CodecError> {
    if samples.len() > MAX_SOURCE_SYNC_SAMPLES {
        return Err(CodecError::TooManyItems(field));
    }
    let mut previous = None;
    let entries = samples
        .iter()
        .map(|sample| {
            if previous.is_some_and(|input| sample.input <= input) || !sample.is_valid() {
                return Err(invalid(field, sample.input.to_string()));
            }
            previous = Some(sample.input);
            Ok(format!(
                "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
                sample.input,
                sample.kind.token(),
                sample.state.token(),
                sample.input_rate_hz,
                sample.output_rate_hz,
                sample.drift_mppm,
                sample.correction_mppm,
                sample.buffer_us,
                sample.target_us,
                sample.resyncs,
                sample.underruns,
                sample.overruns,
                sample.picture_drops,
                sample.picture_repeats,
                sample.restarts,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(entries.join(";"))
}

/// Decodes a sample list from `field`, refusing anything but the exact
/// contract.
pub(super) fn decode_samples(
    field: &'static str,
    value: &str,
) -> Result<Vec<SourceSyncSample>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<_> = value.split(';').collect();
    if entries.len() > MAX_SOURCE_SYNC_SAMPLES {
        return Err(CodecError::TooManyItems(field));
    }
    let mut previous = None;
    entries
        .into_iter()
        .map(|entry| {
            let bad = || invalid(field, entry);
            let parts: Vec<_> = entry.split(':').collect();
            let [
                input,
                kind,
                state,
                input_hz,
                output_hz,
                drift,
                correction,
                buffer,
                target,
                resyncs,
                underruns,
                overruns,
                drops,
                repeats,
                restarts,
            ] = parts[..]
            else {
                return Err(bad());
            };
            let count = |value: &str| parse_canonical_u64(value).ok_or_else(bad);
            let rate = |value: &str| {
                parse_canonical_u64(value)
                    .and_then(|rate| u32::try_from(rate).ok())
                    .ok_or_else(bad)
            };
            let input = parse_canonical_input(input).ok_or_else(bad)?;
            if previous.is_some_and(|last| input <= last) {
                return Err(bad());
            }
            previous = Some(input);
            let sample = SourceSyncSample {
                input,
                kind: SourceSyncKind::from_token(kind).ok_or_else(bad)?,
                state: SourceClockState::from_token(state).ok_or_else(bad)?,
                input_rate_hz: rate(input_hz)?,
                output_rate_hz: rate(output_hz)?,
                drift_mppm: parse_canonical_i64(drift).ok_or_else(bad)?,
                correction_mppm: parse_canonical_i64(correction).ok_or_else(bad)?,
                buffer_us: count(buffer)?,
                target_us: count(target)?,
                resyncs: count(resyncs)?,
                underruns: count(underruns)?,
                overruns: count(overruns)?,
                picture_drops: count(drops)?,
                picture_repeats: count(repeats)?,
                restarts: count(restarts)?,
            };
            if sample.is_valid() {
                Ok(sample)
            } else {
                Err(bad())
            }
        })
        .collect()
}
