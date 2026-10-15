//! Media-file status, operations, and events (Protocol 2.37, IN-003).
//!
//! A [`MediaStatus`] is written as these fields, in this order:
//!
//! * `input`: the media input.
//! * Settings: `trim_in`, `trim_out` (`<frame>` or `-`), `end_action`
//!   (`stop|hold|loop`), `end_trigger_lead` (`<frames>` or `-`),
//!   `speed_basis_points` (`2500..=40000`), `audio_policy`
//!   (`mute|resample`), `audio_stream` (`<stream index>` or `-`, the file's
//!   default track), `growing` (`-` or `<live-edge offset>:<idle refreshes>`),
//!   and `start_frame` (inside the trim range).
//! * Transport: `available` (`0|1`), `state`
//!   (`stopped|playing|paused|waiting|ended`), `position` (source frame),
//!   `position_time` (`<numerator>/<denominator>` seconds in lowest terms),
//!   `playable_end`, `frames`, `complete` (`0|1`), `loops`, `audio_track`
//!   (`<stream index>` or `-`), `tracks`, `source_rate` (`<n>/<d>` or `-`),
//!   and `fault` (`-` or a [`MediaFault`] token).
//!
//! `tracks` is a comma-separated list (at most [`MAX_MEDIA_AUDIO_TRACKS`],
//! ascending unique stream indexes) of
//! `<stream index>:<sample rate>:<channels>:<default 0|1>:<language>:<title>`
//! where `<language>` and `<title>` are `<byte length>:<text>` (length `0`
//! for none). An unavailable source lists no tracks, no selected track, no
//! source rate, and zero frames.
//!
//! `media_events` is a comma-separated list (at most [`MAX_MEDIA_EVENTS`]) of
//! `seeked:<frame>`, `end_trigger:<remaining frames>`, `ended:<action>`,
//! `looped:<count>`, `live_edge_reached:<frame>`, `resumed:<frame>`,
//! `source_grew:<frames>`, `source_completed:<frames>`,
//! `audio_track_changed:<from|->:<to|->`, and `faulted:<fault>`.
//!
//! Every number is a canonical unsigned decimal.

use core::fmt::Write as _;

use crate::{
    CodecError, MAX_MEDIA_AUDIO_TRACKS, MAX_MEDIA_END_TRIGGER_LEAD_FRAMES, MAX_MEDIA_EVENTS,
    MAX_MEDIA_FRAME, MAX_MEDIA_IDLE_COMPLETION_REFRESHES, MAX_MEDIA_LIVE_EDGE_OFFSET_FRAMES,
    MAX_MEDIA_SPEED_BASIS_POINTS, MAX_MEDIA_TRACK_LANGUAGE_BYTES, MAX_MEDIA_TRACK_TITLE_BYTES,
    MIN_MEDIA_LIVE_EDGE_OFFSET_FRAMES, MIN_MEDIA_SPEED_BASIS_POINTS, MediaAudioPolicy,
    MediaAudioTrack, MediaEndAction, MediaEvent, MediaFault, MediaGrowingStatus, MediaOperation,
    MediaSeekTarget, MediaSettingsStatus, MediaStatus, MediaTime, MediaTransportState,
    MediaTransportStatus,
};

use super::playlist::{decode_frame_rate, encode_frame_rate};
use super::value::{parse_canonical_input, parse_canonical_u64};

/// Writes one field of an encoded record.
pub(super) type Push<'a> = dyn FnMut(&'static str, String) -> Result<(), CodecError> + 'a;
/// Takes one required field of a decoded record.
pub(super) type Take<'a> = dyn FnMut(&'static str) -> Result<String, CodecError> + 'a;

/// The fields of a decoded record.
pub(super) trait FieldSource {
    fn required(&mut self, name: &'static str) -> Result<String, CodecError>;
    fn optional(&mut self, name: &'static str) -> Option<String>;
}

/// The name a `media_changed` event gives an operation field (the status
/// fields own the bare names).
pub(super) const fn event_operation_field(name: &'static str) -> &'static str {
    match name.as_bytes() {
        b"frame" => "operation_frame",
        b"time" => "operation_time",
        b"trim_in" => "operation_trim_in",
        b"trim_out" => "operation_trim_out",
        b"speed_basis_points" => "operation_speed_basis_points",
        b"end_action" => "operation_end_action",
        b"end_trigger_lead" => "operation_end_trigger_lead",
        b"audio_track" => "operation_audio_track",
        b"audio_policy" => "operation_audio_policy",
        _ => name,
    }
}

/// A `media_changed` event's operation fields under their event names.
pub(super) struct EventOperationFields<'a>(pub(super) &'a mut dyn FieldSource);

impl FieldSource for EventOperationFields<'_> {
    fn required(&mut self, name: &'static str) -> Result<String, CodecError> {
        self.0.required(event_operation_field(name))
    }

    fn optional(&mut self, name: &'static str) -> Option<String> {
        self.0.optional(event_operation_field(name))
    }
}

fn invalid(field: &'static str, value: impl Into<String>) -> CodecError {
    CodecError::InvalidField {
        field,
        value: value.into(),
    }
}

fn optional_number(value: Option<impl ToString>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| value.to_string())
}

fn parse_optional_u64(field: &'static str, value: String) -> Result<Option<u64>, CodecError> {
    if value == "-" {
        return Ok(None);
    }
    parse_canonical_u64(&value)
        .map(Some)
        .ok_or_else(|| invalid(field, value))
}

fn parse_optional_u32(field: &'static str, value: String) -> Result<Option<u32>, CodecError> {
    match parse_optional_u64(field, value.clone())? {
        None => Ok(None),
        Some(number) => u32::try_from(number)
            .map(Some)
            .map_err(|_| invalid(field, value)),
    }
}

fn parse_u64(field: &'static str, value: String) -> Result<u64, CodecError> {
    parse_canonical_u64(&value).ok_or_else(|| invalid(field, value))
}

fn parse_u32(field: &'static str, value: String) -> Result<u32, CodecError> {
    parse_canonical_u64(&value)
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| invalid(field, value))
}

fn parse_flag(field: &'static str, value: String) -> Result<bool, CodecError> {
    match value.as_str() {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid(field, value)),
    }
}

fn parse_canonical_u128(value: &str) -> Option<u128> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return None;
    }
    value.parse().ok()
}

const fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let next = left % right;
        left = right;
        right = next;
    }
    left
}

/// Enforces the settings bounds shared by the encoder and decoder.
pub(super) fn validate_media_settings(settings: &MediaSettingsStatus) -> Result<(), CodecError> {
    if settings.trim_in > MAX_MEDIA_FRAME
        || settings
            .trim_out
            .is_some_and(|out| out > MAX_MEDIA_FRAME || out <= settings.trim_in)
    {
        return Err(invalid("trim_out", optional_number(settings.trim_out)));
    }
    if settings.start_frame > MAX_MEDIA_FRAME
        || settings.start_frame < settings.trim_in
        || settings
            .trim_out
            .is_some_and(|out| settings.start_frame >= out)
    {
        return Err(invalid("start_frame", settings.start_frame.to_string()));
    }
    if !(MIN_MEDIA_SPEED_BASIS_POINTS..=MAX_MEDIA_SPEED_BASIS_POINTS)
        .contains(&settings.speed_basis_points)
    {
        return Err(invalid(
            "speed_basis_points",
            settings.speed_basis_points.to_string(),
        ));
    }
    if settings
        .end_trigger_lead
        .is_some_and(|lead| lead > MAX_MEDIA_END_TRIGGER_LEAD_FRAMES)
    {
        return Err(invalid(
            "end_trigger_lead",
            optional_number(settings.end_trigger_lead),
        ));
    }
    if let Some(growing) = settings.growing
        && (!(MIN_MEDIA_LIVE_EDGE_OFFSET_FRAMES..=MAX_MEDIA_LIVE_EDGE_OFFSET_FRAMES)
            .contains(&growing.live_edge_offset_frames)
            || growing.idle_completion_refreshes > MAX_MEDIA_IDLE_COMPLETION_REFRESHES)
    {
        return Err(invalid(
            "growing",
            format!(
                "{}:{}",
                growing.live_edge_offset_frames, growing.idle_completion_refreshes
            ),
        ));
    }
    Ok(())
}

fn valid_track_text(text: &str, maximum: usize) -> bool {
    text.len() <= maximum && !text.chars().any(char::is_control)
}

/// Enforces the transport bounds shared by the encoder and decoder.
fn validate_media_transport(transport: &MediaTransportStatus) -> Result<(), CodecError> {
    if transport.tracks.len() > MAX_MEDIA_AUDIO_TRACKS {
        return Err(CodecError::TooManyItems("tracks"));
    }
    for (index, track) in transport.tracks.iter().enumerate() {
        let ascending = index == 0 || transport.tracks[index - 1].stream_index < track.stream_index;
        let texts = track.language.as_deref().is_none_or(|language| {
            !language.is_empty() && valid_track_text(language, MAX_MEDIA_TRACK_LANGUAGE_BYTES)
        }) && track.title.as_deref().is_none_or(|title| {
            !title.is_empty() && valid_track_text(title, MAX_MEDIA_TRACK_TITLE_BYTES)
        });
        if !ascending || !texts {
            return Err(invalid("tracks", track.stream_index.to_string()));
        }
    }
    if let Some(track) = transport.audio_track
        && !transport
            .tracks
            .iter()
            .any(|candidate| candidate.stream_index == track)
    {
        return Err(invalid("audio_track", track.to_string()));
    }
    if transport.position_time.denominator == 0
        || gcd(
            transport.position_time.numerator,
            transport.position_time.denominator,
        ) != 1
    {
        return Err(invalid(
            "position_time",
            format!(
                "{}/{}",
                transport.position_time.numerator, transport.position_time.denominator
            ),
        ));
    }
    if !transport.available
        && (!transport.tracks.is_empty()
            || transport.audio_track.is_some()
            || transport.source_rate.is_some()
            || transport.frames != 0)
    {
        return Err(invalid("available", "0"));
    }
    if transport.available && transport.source_rate.is_none() {
        return Err(invalid("source_rate", "-"));
    }
    Ok(())
}

fn encode_tracks(tracks: &[MediaAudioTrack]) -> String {
    let mut encoded = String::new();
    for (index, track) in tracks.iter().enumerate() {
        if index > 0 {
            encoded.push(',');
        }
        let language = track.language.as_deref().unwrap_or("");
        let title = track.title.as_deref().unwrap_or("");
        write!(
            encoded,
            "{}:{}:{}:{}:{}:{language}:{}:{title}",
            track.stream_index,
            track.sample_rate,
            track.channels,
            u8::from(track.default),
            language.len(),
            title.len(),
        )
        .expect("writing to a string cannot fail");
    }
    encoded
}

/// Splits `<len>:<raw>` off the front of `value`.
fn take_counted(value: &str) -> Option<(&str, &str)> {
    let (length, after) = value.split_once(':')?;
    let length = usize::try_from(parse_canonical_u64(length)?).ok()?;
    let raw = after.get(..length)?;
    Some((raw, &after[length..]))
}

fn decode_tracks(value: &str) -> Result<Vec<MediaAudioTrack>, CodecError> {
    let fail = || invalid("tracks", value);
    let mut tracks = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        if !tracks.is_empty() {
            rest = rest.strip_prefix(',').ok_or_else(fail)?;
        }
        if tracks.len() == MAX_MEDIA_AUDIO_TRACKS {
            return Err(CodecError::TooManyItems("tracks"));
        }
        let mut number = || -> Result<u64, CodecError> {
            let (head, after) = rest.split_once(':').ok_or_else(fail)?;
            rest = after;
            parse_canonical_u64(head).ok_or_else(fail)
        };
        let stream_index = u32::try_from(number()?).map_err(|_| fail())?;
        let sample_rate = u32::try_from(number()?).map_err(|_| fail())?;
        let channels = u16::try_from(number()?).map_err(|_| fail())?;
        let default = match number()? {
            0 => false,
            1 => true,
            _ => return Err(fail()),
        };
        let (language, after) = take_counted(rest).ok_or_else(fail)?;
        rest = after.strip_prefix(':').ok_or_else(fail)?;
        let (title, after) = take_counted(rest).ok_or_else(fail)?;
        rest = after;
        tracks.push(MediaAudioTrack {
            stream_index,
            sample_rate,
            channels,
            language: (!language.is_empty()).then(|| language.to_owned()),
            title: (!title.is_empty()).then(|| title.to_owned()),
            default,
        });
    }
    Ok(tracks)
}

/// Writes one media input's complete status.
pub(super) fn encode_media_status(status: &MediaStatus, push: &mut Push) -> Result<(), CodecError> {
    let settings = &status.settings;
    let transport = &status.transport;
    validate_media_settings(settings)?;
    validate_media_transport(transport)?;
    push("input", status.input.to_string())?;
    push("trim_in", settings.trim_in.to_string())?;
    push("trim_out", optional_number(settings.trim_out))?;
    push("end_action", settings.end_action.token().to_owned())?;
    push(
        "end_trigger_lead",
        optional_number(settings.end_trigger_lead),
    )?;
    push(
        "speed_basis_points",
        settings.speed_basis_points.to_string(),
    )?;
    push("audio_policy", settings.audio_policy.token().to_owned())?;
    push("audio_stream", optional_number(settings.audio_stream))?;
    push(
        "growing",
        settings.growing.map_or_else(
            || "-".to_owned(),
            |growing| {
                format!(
                    "{}:{}",
                    growing.live_edge_offset_frames, growing.idle_completion_refreshes
                )
            },
        ),
    )?;
    push("start_frame", settings.start_frame.to_string())?;
    push("available", u8::from(transport.available).to_string())?;
    push("state", transport.state.token().to_owned())?;
    push("position", transport.position.to_string())?;
    push(
        "position_time",
        format!(
            "{}/{}",
            transport.position_time.numerator, transport.position_time.denominator
        ),
    )?;
    push("playable_end", transport.playable_end.to_string())?;
    push("frames", transport.frames.to_string())?;
    push("complete", u8::from(transport.complete).to_string())?;
    push("loops", transport.loops.to_string())?;
    push("audio_track", optional_number(transport.audio_track))?;
    push("tracks", encode_tracks(&transport.tracks))?;
    push(
        "source_rate",
        transport
            .source_rate
            .map_or_else(|| "-".to_owned(), encode_frame_rate),
    )?;
    push(
        "fault",
        transport
            .fault
            .map_or_else(|| "-".to_owned(), |fault| fault.token().to_owned()),
    )
}

/// Reads one media input's complete status.
pub(super) fn decode_media_status(take: &mut Take) -> Result<MediaStatus, CodecError> {
    let input = {
        let value = take("input")?;
        parse_canonical_input(&value).ok_or_else(|| invalid("input", value))?
    };
    let settings = MediaSettingsStatus {
        trim_in: parse_u64("trim_in", take("trim_in")?)?,
        trim_out: parse_optional_u64("trim_out", take("trim_out")?)?,
        end_action: {
            let value = take("end_action")?;
            MediaEndAction::from_token(&value).ok_or_else(|| invalid("end_action", value))?
        },
        end_trigger_lead: parse_optional_u32("end_trigger_lead", take("end_trigger_lead")?)?,
        speed_basis_points: parse_u32("speed_basis_points", take("speed_basis_points")?)?,
        audio_policy: {
            let value = take("audio_policy")?;
            MediaAudioPolicy::from_token(&value).ok_or_else(|| invalid("audio_policy", value))?
        },
        audio_stream: parse_optional_u32("audio_stream", take("audio_stream")?)?,
        growing: {
            let value = take("growing")?;
            if value == "-" {
                None
            } else {
                let parsed = value.split_once(':').and_then(|(offset, idle)| {
                    Some(MediaGrowingStatus {
                        live_edge_offset_frames: u32::try_from(parse_canonical_u64(offset)?)
                            .ok()?,
                        idle_completion_refreshes: u32::try_from(parse_canonical_u64(idle)?)
                            .ok()?,
                    })
                });
                Some(parsed.ok_or_else(|| invalid("growing", value))?)
            }
        },
        start_frame: parse_u64("start_frame", take("start_frame")?)?,
    };
    validate_media_settings(&settings)?;
    let transport = MediaTransportStatus {
        available: parse_flag("available", take("available")?)?,
        state: {
            let value = take("state")?;
            MediaTransportState::from_token(&value).ok_or_else(|| invalid("state", value))?
        },
        position: parse_u64("position", take("position")?)?,
        position_time: {
            let value = take("position_time")?;
            let parsed = value.split_once('/').and_then(|(numerator, denominator)| {
                Some(MediaTime {
                    numerator: parse_canonical_u128(numerator)?,
                    denominator: parse_canonical_u128(denominator)?,
                })
            });
            parsed.ok_or_else(|| invalid("position_time", value))?
        },
        playable_end: parse_u64("playable_end", take("playable_end")?)?,
        frames: parse_u64("frames", take("frames")?)?,
        complete: parse_flag("complete", take("complete")?)?,
        loops: parse_u64("loops", take("loops")?)?,
        audio_track: parse_optional_u32("audio_track", take("audio_track")?)?,
        tracks: decode_tracks(&take("tracks")?)?,
        source_rate: {
            let value = take("source_rate")?;
            if value == "-" {
                None
            } else {
                Some(decode_frame_rate(value).map_err(|error| match error {
                    CodecError::InvalidField { value, .. } => invalid("source_rate", value),
                    other => other,
                })?)
            }
        },
        fault: {
            let value = take("fault")?;
            if value == "-" {
                None
            } else {
                Some(MediaFault::from_token(&value).ok_or_else(|| invalid("fault", value))?)
            }
        },
    };
    validate_media_transport(&transport)?;
    Ok(MediaStatus {
        input,
        settings,
        transport,
    })
}

/// The command payload name of `operation`.
pub(super) const fn media_payload_name(operation: MediaOperation) -> &'static str {
    match operation {
        MediaOperation::Play => "media_play",
        MediaOperation::Pause => "media_pause",
        MediaOperation::Stop => "media_stop",
        MediaOperation::Seek(_) => "media_seek",
        MediaOperation::CompleteSource => "media_complete_source",
        MediaOperation::Trim { .. } => "media_trim",
        MediaOperation::Rate { .. } => "media_rate",
        MediaOperation::EndAction(_) => "media_end_action",
        MediaOperation::EndTriggerLead(_) => "media_end_trigger_lead",
        MediaOperation::AudioTrack(_) => "media_audio_track",
        MediaOperation::AudioPolicy(_) => "media_audio_policy",
    }
}

/// Writes an operation's own fields (after `payload` and `input` of a
/// command, or `operation` of a `media_changed` event).
pub(super) fn encode_media_operation_fields(
    operation: MediaOperation,
    push: &mut Push,
) -> Result<(), CodecError> {
    match operation {
        MediaOperation::Play
        | MediaOperation::Pause
        | MediaOperation::Stop
        | MediaOperation::CompleteSource => Ok(()),
        MediaOperation::Seek(MediaSeekTarget::Frame(frame)) => push("frame", frame.to_string()),
        MediaOperation::Seek(MediaSeekTarget::Time {
            numerator,
            denominator,
        }) => {
            if denominator == 0 {
                return Err(invalid("time", format!("{numerator}/0")));
            }
            push("time", format!("{numerator}/{denominator}"))
        }
        MediaOperation::Trim { trim_in, trim_out } => {
            if trim_in > MAX_MEDIA_FRAME
                || trim_out.is_some_and(|out| out > MAX_MEDIA_FRAME || out <= trim_in)
            {
                return Err(invalid("trim_out", optional_number(trim_out)));
            }
            push("trim_in", trim_in.to_string())?;
            push("trim_out", optional_number(trim_out))
        }
        MediaOperation::Rate { speed_basis_points } => {
            if !(MIN_MEDIA_SPEED_BASIS_POINTS..=MAX_MEDIA_SPEED_BASIS_POINTS)
                .contains(&speed_basis_points)
            {
                return Err(invalid(
                    "speed_basis_points",
                    speed_basis_points.to_string(),
                ));
            }
            push("speed_basis_points", speed_basis_points.to_string())
        }
        MediaOperation::EndAction(action) => push("end_action", action.token().to_owned()),
        MediaOperation::EndTriggerLead(lead) => {
            if lead.is_some_and(|lead| lead > MAX_MEDIA_END_TRIGGER_LEAD_FRAMES) {
                return Err(invalid("end_trigger_lead", optional_number(lead)));
            }
            push("end_trigger_lead", optional_number(lead))
        }
        MediaOperation::AudioTrack(track) => push("audio_track", optional_number(track)),
        MediaOperation::AudioPolicy(policy) => push("audio_policy", policy.token().to_owned()),
    }
}

/// Reads the operation whose payload name is `name` and its fields, or
/// `None` when `name` is not a media payload.
pub(super) fn decode_media_operation(
    name: &str,
    fields: &mut dyn FieldSource,
) -> Result<Option<MediaOperation>, CodecError> {
    Ok(Some(match name {
        "media_play" => MediaOperation::Play,
        "media_pause" => MediaOperation::Pause,
        "media_stop" => MediaOperation::Stop,
        "media_complete_source" => MediaOperation::CompleteSource,
        "media_seek" => match (fields.optional("frame"), fields.optional("time")) {
            (Some(frame), None) => {
                MediaOperation::Seek(MediaSeekTarget::Frame(parse_u64("frame", frame)?))
            }
            (None, Some(time)) => {
                let parsed = time.split_once('/').and_then(|(numerator, denominator)| {
                    let numerator = parse_canonical_u64(numerator)?;
                    let denominator =
                        parse_canonical_u64(denominator).filter(|value| *value != 0)?;
                    Some(MediaSeekTarget::Time {
                        numerator,
                        denominator,
                    })
                });
                MediaOperation::Seek(parsed.ok_or_else(|| invalid("time", time))?)
            }
            (Some(_), Some(time)) => return Err(invalid("time", time)),
            (None, None) => return Err(CodecError::MissingField("frame")),
        },
        "media_trim" => {
            let trim_in = parse_u64("trim_in", fields.required("trim_in")?)?;
            let trim_out = parse_optional_u64("trim_out", fields.required("trim_out")?)?;
            if trim_in > MAX_MEDIA_FRAME
                || trim_out.is_some_and(|out| out > MAX_MEDIA_FRAME || out <= trim_in)
            {
                return Err(invalid("trim_out", optional_number(trim_out)));
            }
            MediaOperation::Trim { trim_in, trim_out }
        }
        "media_rate" => {
            let speed_basis_points =
                parse_u32("speed_basis_points", fields.required("speed_basis_points")?)?;
            if !(MIN_MEDIA_SPEED_BASIS_POINTS..=MAX_MEDIA_SPEED_BASIS_POINTS)
                .contains(&speed_basis_points)
            {
                return Err(invalid(
                    "speed_basis_points",
                    speed_basis_points.to_string(),
                ));
            }
            MediaOperation::Rate { speed_basis_points }
        }
        "media_end_action" => {
            let value = fields.required("end_action")?;
            MediaOperation::EndAction(
                MediaEndAction::from_token(&value).ok_or_else(|| invalid("end_action", value))?,
            )
        }
        "media_end_trigger_lead" => {
            let lead =
                parse_optional_u32("end_trigger_lead", fields.required("end_trigger_lead")?)?;
            if lead.is_some_and(|lead| lead > MAX_MEDIA_END_TRIGGER_LEAD_FRAMES) {
                return Err(invalid("end_trigger_lead", optional_number(lead)));
            }
            MediaOperation::EndTriggerLead(lead)
        }
        "media_audio_track" => MediaOperation::AudioTrack(parse_optional_u32(
            "audio_track",
            fields.required("audio_track")?,
        )?),
        "media_audio_policy" => {
            let value = fields.required("audio_policy")?;
            MediaOperation::AudioPolicy(
                MediaAudioPolicy::from_token(&value)
                    .ok_or_else(|| invalid("audio_policy", value))?,
            )
        }
        _ => return Ok(None),
    }))
}

/// The `media_changed` `operation` token's command payload name.
pub(super) fn operation_payload_name(token: &str) -> Option<&'static str> {
    Some(match token {
        "play" => "media_play",
        "pause" => "media_pause",
        "stop" => "media_stop",
        "seek" => "media_seek",
        "complete_source" => "media_complete_source",
        "trim" => "media_trim",
        "rate" => "media_rate",
        "end_action" => "media_end_action",
        "end_trigger_lead" => "media_end_trigger_lead",
        "audio_track" => "media_audio_track",
        "audio_policy" => "media_audio_policy",
        _ => return None,
    })
}

fn encode_event(event: MediaEvent) -> String {
    match event {
        MediaEvent::Seeked { frame } => format!("seeked:{frame}"),
        MediaEvent::EndTrigger { remaining_frames } => format!("end_trigger:{remaining_frames}"),
        MediaEvent::Ended { action } => format!("ended:{}", action.token()),
        MediaEvent::Looped { count } => format!("looped:{count}"),
        MediaEvent::LiveEdgeReached { frame } => format!("live_edge_reached:{frame}"),
        MediaEvent::Resumed { frame } => format!("resumed:{frame}"),
        MediaEvent::SourceGrew { frames } => format!("source_grew:{frames}"),
        MediaEvent::SourceCompleted { frames } => format!("source_completed:{frames}"),
        MediaEvent::AudioTrackChanged { from, to } => format!(
            "audio_track_changed:{}:{}",
            optional_number(from),
            optional_number(to)
        ),
        MediaEvent::Faulted { fault } => format!("faulted:{}", fault.token()),
    }
}

/// Encodes a runtime record's `media_events`.
pub(super) fn encode_media_events(events: &[MediaEvent]) -> Result<String, CodecError> {
    if events.len() > MAX_MEDIA_EVENTS {
        return Err(CodecError::TooManyItems("media_events"));
    }
    Ok(events
        .iter()
        .map(|event| encode_event(*event))
        .collect::<Vec<_>>()
        .join(","))
}

fn decode_event(value: &str) -> Option<MediaEvent> {
    let (name, argument) = value.split_once(':')?;
    let number = || parse_canonical_u64(argument);
    let optional = |value: &str| -> Option<Option<u32>> {
        if value == "-" {
            Some(None)
        } else {
            u32::try_from(parse_canonical_u64(value)?).ok().map(Some)
        }
    };
    Some(match name {
        "seeked" => MediaEvent::Seeked { frame: number()? },
        "end_trigger" => MediaEvent::EndTrigger {
            remaining_frames: number()?,
        },
        "ended" => MediaEvent::Ended {
            action: MediaEndAction::from_token(argument)?,
        },
        "looped" => MediaEvent::Looped { count: number()? },
        "live_edge_reached" => MediaEvent::LiveEdgeReached { frame: number()? },
        "resumed" => MediaEvent::Resumed { frame: number()? },
        "source_grew" => MediaEvent::SourceGrew { frames: number()? },
        "source_completed" => MediaEvent::SourceCompleted { frames: number()? },
        "audio_track_changed" => {
            let (from, to) = argument.split_once(':')?;
            MediaEvent::AudioTrackChanged {
                from: optional(from)?,
                to: optional(to)?,
            }
        }
        "faulted" => MediaEvent::Faulted {
            fault: MediaFault::from_token(argument)?,
        },
        _ => return None,
    })
}

/// Decodes a runtime record's `media_events`.
pub(super) fn decode_media_events(value: &str) -> Result<Vec<MediaEvent>, CodecError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let events = value
        .split(',')
        .map(|item| decode_event(item).ok_or_else(|| invalid("media_events", value)))
        .collect::<Result<Vec<_>, _>>()?;
    if events.len() > MAX_MEDIA_EVENTS {
        return Err(CodecError::TooManyItems("media_events"));
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_round_trip_with_counted_text() {
        let tracks = vec![
            MediaAudioTrack {
                stream_index: 1,
                sample_rate: 48_000,
                channels: 2,
                language: Some("eng".into()),
                title: Some("Main, mix: 5.1".into()),
                default: true,
            },
            MediaAudioTrack {
                stream_index: 3,
                sample_rate: 44_100,
                channels: 1,
                language: None,
                title: None,
                default: false,
            },
        ];
        let encoded = encode_tracks(&tracks);
        assert_eq!(
            encoded,
            "1:48000:2:1:3:eng:14:Main, mix: 5.1,3:44100:1:0:0::0:"
        );
        assert_eq!(decode_tracks(&encoded).unwrap(), tracks);
        assert_eq!(decode_tracks("").unwrap(), Vec::new());
        for bad in [
            "1:48000:2:2:0::0:",
            "01:48000:2:1:0::0:",
            "1:48000:2:1:4:eng:0:",
        ] {
            assert!(decode_tracks(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn events_round_trip() {
        let events = vec![
            MediaEvent::Seeked { frame: 4 },
            MediaEvent::EndTrigger {
                remaining_frames: 30,
            },
            MediaEvent::Ended {
                action: MediaEndAction::Loop,
            },
            MediaEvent::Looped { count: 2 },
            MediaEvent::AudioTrackChanged {
                from: None,
                to: Some(2),
            },
            MediaEvent::Faulted {
                fault: MediaFault::SourceTruncated,
            },
        ];
        let encoded = encode_media_events(&events).unwrap();
        assert_eq!(
            encoded,
            "seeked:4,end_trigger:30,ended:loop,looped:2,audio_track_changed:-:2,faulted:source_truncated"
        );
        assert_eq!(decode_media_events(&encoded).unwrap(), events);
        assert!(decode_media_events("ended:pause").is_err());
        assert!(decode_media_events("looped:01").is_err());
    }
}
