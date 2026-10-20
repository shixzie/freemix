//! Strip and Master DSP chains in `project.json` (schema 65, AU-005).
//!
//! Every entry of `input_audio_strips` carries `"dsp"`, and the project
//! carries `"master_audio_dsp"`: `null` for the default chain (every stage
//! off), otherwise the whole chain:
//!
//! ```text
//! {"bypass": false,
//!  "high_pass": {"enabled": true, "frequency_hz": 80},
//!  "eq": {"enabled": true, "bands": [
//!    {"enabled": true, "kind": "low_shelf", "frequency_hz": 100, "gain_milli_db": -2000, "q_milli": 707},
//!    ... five bands ...]},
//!  "gate": {"enabled": true, "threshold_milli_db": -50000, "range_milli_db": 40000,
//!           "attack_us": 1000, "hold_us": 50000, "release_us": 200000},
//!  "compressor": {"enabled": true, "threshold_milli_db": -18000, "ratio_milli": 3000,
//!                 "attack_us": 10000, "release_us": 150000, "knee_milli_db": 6000,
//!                 "makeup_milli_db": 0},
//!  "limiter": {"enabled": false, "ceiling_milli_db": -1000, "release_us": 50000}}
//! ```
//!
//! Every field is required, band `kind` is `low_shelf`, `peak`, or
//! `high_shelf`, there are exactly five bands, and every value is checked
//! against [`StripDsp::validate`]; an explicit default chain is written as
//! `null` only, so the encoding is canonical.

use core::fmt::Write as _;

use fm_types::strip_dsp::{
    EqBandKind, STRIP_EQ_BAND_COUNT, StripCompressor, StripDsp, StripEq, StripEqBand, StripGate,
    StripHighPass, StripLimiter,
};

use super::{DecodeError, Object, Value, parse_array, parse_optional, syntax, unknown_enum};

/// Writes `null` for the default chain or the chain object.
pub(crate) fn write_strip_dsp(output: &mut String, dsp: &StripDsp) {
    if dsp.is_default() {
        output.push_str("null");
        return;
    }
    write!(
        output,
        "{{\"bypass\": {}, \"high_pass\": {{\"enabled\": {}, \"frequency_hz\": {}}}, \
         \"eq\": {{\"enabled\": {}, \"bands\": [",
        dsp.bypass, dsp.high_pass.enabled, dsp.high_pass.frequency_hz, dsp.eq.enabled
    )
    .expect("writing to a string cannot fail");
    for (index, band) in dsp.eq.bands.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(
            output,
            "{{\"enabled\": {}, \"kind\": \"{}\", \"frequency_hz\": {}, \"gain_milli_db\": {}, \
             \"q_milli\": {}}}",
            band.enabled,
            band.kind.token(),
            band.frequency_hz,
            band.gain_millidb,
            band.q_milli
        )
        .expect("writing to a string cannot fail");
    }
    let gate = &dsp.gate;
    let compressor = &dsp.compressor;
    let limiter = &dsp.limiter;
    write!(
        output,
        "]}}, \"gate\": {{\"enabled\": {}, \"threshold_milli_db\": {}, \"range_milli_db\": {}, \
         \"attack_us\": {}, \"hold_us\": {}, \"release_us\": {}}}, \"compressor\": \
         {{\"enabled\": {}, \"threshold_milli_db\": {}, \"ratio_milli\": {}, \"attack_us\": {}, \
         \"release_us\": {}, \"knee_milli_db\": {}, \"makeup_milli_db\": {}}}, \"limiter\": \
         {{\"enabled\": {}, \"ceiling_milli_db\": {}, \"release_us\": {}}}}}",
        gate.enabled,
        gate.threshold_millidb,
        gate.range_millidb,
        gate.attack_us,
        gate.hold_us,
        gate.release_us,
        compressor.enabled,
        compressor.threshold_millidb,
        compressor.ratio_milli,
        compressor.attack_us,
        compressor.release_us,
        compressor.knee_millidb,
        compressor.makeup_millidb,
        limiter.enabled,
        limiter.ceiling_millidb,
        limiter.release_us,
    )
    .expect("writing to a string cannot fail");
}

/// Parses `null` (the default chain) or a chain object.
pub(crate) fn parse_strip_dsp(
    value: Value,
    context: &'static str,
) -> Result<StripDsp, DecodeError> {
    let Some(dsp) = parse_optional(value, |value| parse_chain(value, context))? else {
        return Ok(StripDsp::DEFAULT);
    };
    if dsp.is_default() {
        return Err(syntax(format!(
            "{context} writes the default chain as null"
        )));
    }
    dsp.validate()
        .map_err(|error| syntax(format!("{context}: {error}")))?;
    Ok(dsp)
}

fn parse_chain(value: Value, context: &'static str) -> Result<StripDsp, DecodeError> {
    let mut object = Object::new(value, context)?;
    let bypass = object.boolean("bypass")?;
    let mut high_pass = Object::new(object.take("high_pass")?, "strip DSP high_pass")?;
    let high_pass_value = StripHighPass {
        enabled: high_pass.boolean("enabled")?,
        frequency_hz: high_pass.u32("frequency_hz")?,
    };
    high_pass.finish()?;
    let mut eq = Object::new(object.take("eq")?, "strip DSP eq")?;
    let eq_enabled = eq.boolean("enabled")?;
    let bands = parse_array(eq.take("bands")?, "strip DSP eq bands", |value| {
        let mut band = Object::new(value, "strip DSP eq band")?;
        let enabled = band.boolean("enabled")?;
        let token = band.string("kind")?;
        let kind = EqBandKind::from_token(&token)
            .ok_or_else(|| unknown_enum("strip DSP eq band kind", &token))?;
        let parsed = StripEqBand {
            enabled,
            kind,
            frequency_hz: band.u32("frequency_hz")?,
            gain_millidb: band.i32("gain_milli_db")?,
            q_milli: band.u32("q_milli")?,
        };
        band.finish()?;
        Ok(parsed)
    })?;
    eq.finish()?;
    let bands: [StripEqBand; STRIP_EQ_BAND_COUNT] = bands.try_into().map_err(|_| {
        syntax(format!(
            "strip DSP eq must have exactly {STRIP_EQ_BAND_COUNT} bands"
        ))
    })?;
    let mut gate = Object::new(object.take("gate")?, "strip DSP gate")?;
    let gate_value = StripGate {
        enabled: gate.boolean("enabled")?,
        threshold_millidb: gate.i32("threshold_milli_db")?,
        range_millidb: gate.u32("range_milli_db")?,
        attack_us: gate.u32("attack_us")?,
        hold_us: gate.u32("hold_us")?,
        release_us: gate.u32("release_us")?,
    };
    gate.finish()?;
    let mut compressor = Object::new(object.take("compressor")?, "strip DSP compressor")?;
    let compressor_value = StripCompressor {
        enabled: compressor.boolean("enabled")?,
        threshold_millidb: compressor.i32("threshold_milli_db")?,
        ratio_milli: compressor.u32("ratio_milli")?,
        attack_us: compressor.u32("attack_us")?,
        release_us: compressor.u32("release_us")?,
        knee_millidb: compressor.u32("knee_milli_db")?,
        makeup_millidb: compressor.u32("makeup_milli_db")?,
    };
    compressor.finish()?;
    let mut limiter = Object::new(object.take("limiter")?, "strip DSP limiter")?;
    let limiter_value = StripLimiter {
        enabled: limiter.boolean("enabled")?,
        ceiling_millidb: limiter.i32("ceiling_milli_db")?,
        release_us: limiter.u32("release_us")?,
    };
    limiter.finish()?;
    object.finish()?;
    Ok(StripDsp {
        bypass,
        high_pass: high_pass_value,
        eq: StripEq {
            enabled: eq_enabled,
            bands,
        },
        gate: gate_value,
        compressor: compressor_value,
        limiter: limiter_value,
    })
}
