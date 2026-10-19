//! The operator cue lists in `project.json` (schema 48, RC-017).
//!
//! ```text
//! "cue_lists": [
//!   {"id": 1, "name": "Show", "cues": [
//!     {"id": 1, "name": "Open", "auto_follow": false, "actions": [
//!       {"type": "take", "input": 3, "effect": "fade", "duration_frames": 30},
//!       {"type": "wait", "frames": 30},
//!       {"type": "title_on", "input": 5, "channel": 2}
//!     ]}
//!   ], "state": {"standby": 1, "live": null, "pending": null, "played": [],
//!                "fault": null, "armed": null}}
//! ]
//! ```
//!
//! A fault is `{"cue", "action", "reason", "message"}`: the action refused
//! when its wait ended, and (schema 57) what it was refused with, as a
//! script run's fault says it (see [`super::script`]). `armed` (schema 58)
//! is `null` or `{"cue", "due_frame"}`: a GO armed to fire on that engine
//! frame.
//!
//! Action types: `preview` (`input`), `take` (`input` or `null` for the
//! current Preview, `effect` a [`fm_model::TransitionEffect`] token or
//! `"stinger"` with a `slot`, `duration_frames`), `overlay_on` (`channel`,
//! `input`), `overlay_off` (`channel`), `title_on` (`input`, `channel`),
//! `title_text` (`input`, `field`, `text`), `title_off` (`input`),
//! `fade_to_black` (`active`, `duration_frames`), `playlist_start`,
//! `playlist_stop`, `audio_gain` (`input`, `gain_milli_db`), `audio_mute`
//! (`input`, `muted`), `record` (`recorder`, `start`), `stream` (`target`,
//! `start`), `media_play`, `media_pause`, and `media_restart` (`input`),
//! `media_seek` (`input`, `frame`), `wait` (`frames`), `delay`
//! (`millis`), and `run_script` (`script`, a project script id; schema 55).
//! Bounds and references are reported by project validation.

use core::fmt::Write as _;

use fm_model::{
    Cue, CueAction, CueArmed, CueFault, CueList, CueMediaOperation, CuePending, CueRunState,
    RecorderSlot, StingerSlotNumber, StreamTargetId, TitleFieldId, TransitionEffect,
};
use fm_types::InputId;

use super::{DecodeError, Object, Value, escape_string, parse_array, parse_optional, syntax};

fn optional_id(value: Option<u64>) -> String {
    value.map_or_else(|| "null".to_owned(), |id| id.to_string())
}

/// Writes `,\n    "cue_lists": [...]` after the playlist.
pub(crate) fn write_cue_lists(output: &mut String, lists: &[CueList]) {
    output.push_str(",\n    \"cue_lists\": [");
    for (index, list) in lists.iter().enumerate() {
        output.push_str(if index == 0 { "\n      " } else { ",\n      " });
        write!(output, "{{\"id\": {}, \"name\": \"", list.id)
            .expect("writing to a string cannot fail");
        escape_string(output, &list.name);
        output.push_str("\", \"cues\": [");
        for (cue_index, cue) in list.cues.iter().enumerate() {
            output.push_str(if cue_index == 0 {
                "\n        "
            } else {
                ",\n        "
            });
            write_cue(output, cue);
        }
        if !list.cues.is_empty() {
            output.push_str("\n      ");
        }
        output.push_str("], \"state\": ");
        write_state(output, &list.state);
        output.push('}');
    }
    if !lists.is_empty() {
        output.push_str("\n    ");
    }
    output.push(']');
}

fn write_cue(output: &mut String, cue: &Cue) {
    write!(output, "{{\"id\": {}, \"name\": \"", cue.id).expect("writing to a string cannot fail");
    escape_string(output, &cue.name);
    write!(
        output,
        "\", \"auto_follow\": {}, \"actions\": [",
        cue.auto_follow
    )
    .expect("writing to a string cannot fail");
    for (index, action) in cue.actions.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write_action(output, action);
    }
    output.push_str("]}");
}

fn write_action(output: &mut String, action: &CueAction) {
    let result = match action {
        CueAction::Preview { input } => {
            write!(output, "{{\"type\": \"preview\", \"input\": {input}}}")
        }
        CueAction::Take {
            input,
            effect,
            duration_frames,
        } => {
            let input = input.map_or_else(|| "null".to_owned(), |input| input.to_string());
            let effect = match effect {
                TransitionEffect::Stinger(slot) => {
                    format!("\"stinger\", \"slot\": {}", slot.number())
                }
                effect => format!(
                    "\"{}\"",
                    effect.token().expect("built-in effects have tokens")
                ),
            };
            write!(
                output,
                "{{\"type\": \"take\", \"input\": {input}, \"effect\": {effect}, \
                 \"duration_frames\": {duration_frames}}}"
            )
        }
        CueAction::OverlayOn { channel, input } => write!(
            output,
            "{{\"type\": \"overlay_on\", \"channel\": {channel}, \"input\": {input}}}"
        ),
        CueAction::OverlayOff { channel } => {
            write!(
                output,
                "{{\"type\": \"overlay_off\", \"channel\": {channel}}}"
            )
        }
        CueAction::TitleOn { input, channel } => write!(
            output,
            "{{\"type\": \"title_on\", \"input\": {input}, \"channel\": {channel}}}"
        ),
        CueAction::TitleText { input, field, text } => {
            write!(
                output,
                "{{\"type\": \"title_text\", \"input\": {input}, \"field\": {field}, \"text\": \""
            )
            .expect("writing to a string cannot fail");
            escape_string(output, text);
            write!(output, "\"}}")
        }
        CueAction::TitleOff { input } => {
            write!(output, "{{\"type\": \"title_off\", \"input\": {input}}}")
        }
        CueAction::FadeToBlack {
            active,
            duration_frames,
        } => write!(
            output,
            "{{\"type\": \"fade_to_black\", \"active\": {active}, \
             \"duration_frames\": {duration_frames}}}"
        ),
        CueAction::PlaylistStart => write!(output, "{{\"type\": \"playlist_start\"}}"),
        CueAction::PlaylistStop => write!(output, "{{\"type\": \"playlist_stop\"}}"),
        CueAction::AudioGain {
            input,
            gain_millidb,
        } => write!(
            output,
            "{{\"type\": \"audio_gain\", \"input\": {input}, \"gain_milli_db\": {gain_millidb}}}"
        ),
        CueAction::AudioMute { input, muted } => write!(
            output,
            "{{\"type\": \"audio_mute\", \"input\": {input}, \"muted\": {muted}}}"
        ),
        CueAction::Record { recorder, start } => write!(
            output,
            "{{\"type\": \"record\", \"recorder\": {}, \"start\": {start}}}",
            recorder.get()
        ),
        CueAction::Stream { target, start } => write!(
            output,
            "{{\"type\": \"stream\", \"target\": {target}, \"start\": {start}}}"
        ),
        CueAction::Media { input, operation } => write_media(output, *input, operation),
        CueAction::Wait { frames } => {
            write!(output, "{{\"type\": \"wait\", \"frames\": {frames}}}")
        }
        CueAction::Delay { millis } => {
            write!(output, "{{\"type\": \"delay\", \"millis\": {millis}}}")
        }
        CueAction::RunScript { script } => {
            write!(output, "{{\"type\": \"run_script\", \"script\": {script}}}")
        }
    };
    result.expect("writing to a string cannot fail");
}

fn write_media(
    output: &mut String,
    input: InputId,
    operation: &CueMediaOperation,
) -> core::fmt::Result {
    match operation {
        CueMediaOperation::Play => {
            write!(output, "{{\"type\": \"media_play\", \"input\": {input}}}")
        }
        CueMediaOperation::Pause => {
            write!(output, "{{\"type\": \"media_pause\", \"input\": {input}}}")
        }
        CueMediaOperation::Restart => write!(
            output,
            "{{\"type\": \"media_restart\", \"input\": {input}}}"
        ),
        CueMediaOperation::Seek { frame } => write!(
            output,
            "{{\"type\": \"media_seek\", \"input\": {input}, \"frame\": {frame}}}"
        ),
    }
}

fn write_state(output: &mut String, state: &CueRunState) {
    write!(
        output,
        "{{\"standby\": {}, \"live\": {}, \"pending\": ",
        optional_id(state.standby),
        optional_id(state.live)
    )
    .expect("writing to a string cannot fail");
    match state.pending {
        None => output.push_str("null"),
        Some(pending) => write!(
            output,
            "{{\"cue\": {}, \"action\": {}, \"due_frame\": {}}}",
            pending.cue, pending.action, pending.due_frame
        )
        .expect("writing to a string cannot fail"),
    }
    output.push_str(", \"played\": [");
    for (index, cue) in state.played.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(output, "{cue}").expect("writing to a string cannot fail");
    }
    output.push_str("], \"fault\": ");
    match &state.fault {
        None => output.push_str("null"),
        Some(fault) => {
            write!(
                output,
                "{{\"cue\": {}, \"action\": {}",
                fault.cue, fault.action
            )
            .expect("writing to a string cannot fail");
            super::script::write_fault_detail(output, &fault.detail);
            output.push('}');
        }
    }
    output.push_str(", \"armed\": ");
    match state.armed {
        None => output.push_str("null"),
        Some(armed) => write!(
            output,
            "{{\"cue\": {}, \"due_frame\": {}}}",
            armed.cue, armed.due_frame
        )
        .expect("writing to a string cannot fail"),
    }
    output.push('}');
}

/// Parses `"cue_lists"` from the project object.
pub(crate) fn parse_cue_lists(object: &mut Object) -> Result<Vec<CueList>, DecodeError> {
    parse_array(object.take("cue_lists")?, "cue_lists", parse_cue_list)
}

fn parse_cue_list(value: Value) -> Result<CueList, DecodeError> {
    let mut object = Object::new(value, "cue list")?;
    let id = object.u64("id")?;
    let name = object.string("name")?;
    let cues = parse_array(object.take("cues")?, "cue list cues", parse_cue)?;
    let state = parse_state(object.take("state")?)?;
    object.finish()?;
    Ok(CueList {
        id,
        name,
        cues,
        state,
    })
}

fn parse_cue(value: Value) -> Result<Cue, DecodeError> {
    let mut object = Object::new(value, "cue")?;
    let id = object.u64("id")?;
    let name = object.string("name")?;
    let auto_follow = object.boolean("auto_follow")?;
    let actions = parse_array(object.take("actions")?, "cue actions", parse_action)?;
    object.finish()?;
    Ok(Cue {
        id,
        name,
        actions,
        auto_follow,
    })
}

fn input(object: &mut Object) -> Result<InputId, DecodeError> {
    Ok(InputId::new(object.nonzero_u128("input")?))
}

#[allow(clippy::too_many_lines)] // one arm per action type
fn parse_action(value: Value) -> Result<CueAction, DecodeError> {
    let mut object = Object::new(value, "cue action")?;
    let action = match object.string("type")?.as_str() {
        "preview" => CueAction::Preview {
            input: input(&mut object)?,
        },
        "take" => {
            let input = parse_optional(object.take("input")?, |value| {
                let Value::Number(value) = value else {
                    return Err(syntax("cue take input must be an input id or null"));
                };
                core::num::NonZeroU128::new(value)
                    .map(InputId::new)
                    .ok_or_else(|| syntax("cue take input must be nonzero"))
            })?;
            let effect = match object.string("effect")?.as_str() {
                "stinger" => TransitionEffect::Stinger(
                    StingerSlotNumber::new(object.u8("slot")?)
                        .ok_or_else(|| syntax("cue stinger slot must be 1 through 8"))?,
                ),
                token => TransitionEffect::from_token(token)
                    .ok_or_else(|| syntax(format!("unknown cue transition effect `{token}`")))?,
            };
            CueAction::Take {
                input,
                effect,
                duration_frames: object.u32("duration_frames")?,
            }
        }
        "overlay_on" => CueAction::OverlayOn {
            channel: object.u8("channel")?,
            input: input(&mut object)?,
        },
        "overlay_off" => CueAction::OverlayOff {
            channel: object.u8("channel")?,
        },
        "title_on" => CueAction::TitleOn {
            input: input(&mut object)?,
            channel: object.u8("channel")?,
        },
        "title_text" => CueAction::TitleText {
            input: input(&mut object)?,
            field: TitleFieldId::new(object.nonzero_u128("field")?),
            text: object.string("text")?,
        },
        "title_off" => CueAction::TitleOff {
            input: input(&mut object)?,
        },
        "fade_to_black" => CueAction::FadeToBlack {
            active: object.boolean("active")?,
            duration_frames: object.u32("duration_frames")?,
        },
        "playlist_start" => CueAction::PlaylistStart,
        "playlist_stop" => CueAction::PlaylistStop,
        "audio_gain" => CueAction::AudioGain {
            input: input(&mut object)?,
            gain_millidb: object.i32("gain_milli_db")?,
        },
        "audio_mute" => CueAction::AudioMute {
            input: input(&mut object)?,
            muted: object.boolean("muted")?,
        },
        "record" => CueAction::Record {
            recorder: RecorderSlot::new(object.u8("recorder")?)
                .map_err(|_| syntax("cue recorder must be 1 or 2"))?,
            start: object.boolean("start")?,
        },
        "stream" => CueAction::Stream {
            target: StreamTargetId::new(object.nonzero_u128("target")?),
            start: object.boolean("start")?,
        },
        "media_play" => CueAction::Media {
            input: input(&mut object)?,
            operation: CueMediaOperation::Play,
        },
        "media_pause" => CueAction::Media {
            input: input(&mut object)?,
            operation: CueMediaOperation::Pause,
        },
        "media_restart" => CueAction::Media {
            input: input(&mut object)?,
            operation: CueMediaOperation::Restart,
        },
        "media_seek" => CueAction::Media {
            input: input(&mut object)?,
            operation: CueMediaOperation::Seek {
                frame: object.u64("frame")?,
            },
        },
        "wait" => CueAction::Wait {
            frames: object.u32("frames")?,
        },
        "delay" => CueAction::Delay {
            millis: object.u32("millis")?,
        },
        "run_script" => CueAction::RunScript {
            script: object.u64("script")?,
        },
        value => return Err(super::unknown_enum("cue action", value)),
    };
    object.finish()?;
    Ok(action)
}

fn optional_u64(value: Value, context: &str) -> Result<Option<u64>, DecodeError> {
    parse_optional(value, |value| {
        let Value::Number(value) = value else {
            return Err(syntax(format!(
                "{context} must be an unsigned integer or null"
            )));
        };
        u64::try_from(value).map_err(|_| syntax(format!("{context} exceeds u64")))
    })
}

fn parse_state(value: Value) -> Result<CueRunState, DecodeError> {
    let mut object = Object::new(value, "cue list state")?;
    let standby = optional_u64(object.take("standby")?, "cue standby")?;
    let live = optional_u64(object.take("live")?, "cue live")?;
    let pending = parse_optional(object.take("pending")?, |value| {
        let mut pending = Object::new(value, "cue pending")?;
        let parsed = CuePending {
            cue: pending.u64("cue")?,
            action: usize::try_from(pending.u32("action")?)
                .map_err(|_| syntax("cue pending action exceeds usize"))?,
            due_frame: pending.u64("due_frame")?,
        };
        pending.finish()?;
        Ok(parsed)
    })?;
    let played = parse_array(object.take("played")?, "cue played", |value| {
        let Value::Number(value) = value else {
            return Err(syntax("cue played entries must be unsigned integers"));
        };
        u64::try_from(value).map_err(|_| syntax("cue played entry exceeds u64"))
    })?;
    let fault = parse_optional(object.take("fault")?, |value| {
        let mut fault = Object::new(value, "cue fault")?;
        let parsed = CueFault {
            cue: fault.u64("cue")?,
            action: usize::try_from(fault.u32("action")?)
                .map_err(|_| syntax("cue fault action exceeds usize"))?,
            detail: super::script::parse_fault_detail(&mut fault)?,
        };
        fault.finish()?;
        Ok(parsed)
    })?;
    let armed = parse_optional(object.take("armed")?, |value| {
        let mut armed = Object::new(value, "cue armed")?;
        let parsed = CueArmed {
            cue: armed.u64("cue")?,
            due_frame: armed.u64("due_frame")?,
        };
        armed.finish()?;
        Ok(parsed)
    })?;
    object.finish()?;
    Ok(CueRunState {
        standby,
        live,
        pending,
        played,
        fault,
        armed,
    })
}
