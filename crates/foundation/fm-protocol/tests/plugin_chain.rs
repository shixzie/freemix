//! AU-006, Protocol 2.128: an audio plugin chain's command and event round
//! trip exactly, and malformed chains are refused strictly.

use fm_protocol::{
    AudioDspTarget, AudioMixerOperation, CommandMessage, CommandPayload, EventCursor, EventMessage,
    EventPayload, PluginChainSlot, ProtocolVersion, WireInputId, WireMessage, decode_line,
    encode_line,
};

fn input(value: u128) -> WireInputId {
    WireInputId::new(core::num::NonZeroU128::new(value).unwrap())
}

fn command(payload: CommandPayload) -> WireMessage {
    WireMessage::Command(CommandMessage {
        protocol: fm_protocol::CURRENT_PROTOCOL_VERSION,
        id: "a".to_owned(),
        idempotency_key: "k".to_owned(),
        expected_revision: None,
        deadline_ms: None,
        payload,
    })
}

fn event(payload: EventPayload) -> WireMessage {
    WireMessage::Event(EventMessage {
        cursor: EventCursor {
            engine: fm_protocol::EngineIdentity {
                engine_id: "e".into(),
                state_epoch: 1,
                log_id: "l".into(),
            },
            revision: 4,
        },
        edit_history: fm_protocol::EditHistoryStatus::EMPTY,
        payload,
    })
}

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

/// The unpadded base64url of `data`, as the wire carries a slot's state.
fn base64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::new();
    for chunk in data.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        let characters = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for index in 0..characters {
            let shift = 18 - index * 6;
            encoded.push(ALPHABET[((value >> shift) & 0x3f) as usize] as char);
        }
    }
    encoded
}

fn trim(milli_db: i32) -> PluginChainSlot {
    PluginChainSlot {
        plugin: "freemix.reference.trim".to_owned(),
        bypassed: false,
        latency_frames: 0,
        state: milli_db.to_le_bytes().to_vec(),
    }
}

#[test]
fn plugin_chains_round_trip_exactly() {
    let operation = AudioMixerOperation::SetPluginChain {
        target: AudioDspTarget::Input(input(7)),
        slots: vec![
            trim(-12_000),
            PluginChainSlot {
                plugin: "freemix.reference.polarity".to_owned(),
                bypassed: true,
                latency_frames: 2,
                state: Vec::new(),
            },
        ],
    };
    let state = base64url(&(-12_000_i32).to_le_bytes());
    let line = round_trip(&command(CommandPayload::AudioMixer(operation.clone())));
    assert_eq!(
        line,
        format!(
            "command\tprotocol=2.142\tid=a\tidempotency_key=k\tpayload=audio_plugin_chain\t\
             target=7\tplugins=freemix.reference.trim%3A0%3A0%3A{state}%2C\
             freemix.reference.polarity%3A1%3A2%3A\n"
        )
    );
    let line = round_trip(&event(EventPayload::AudioMixerChanged(operation.clone())));
    assert!(
        line.contains("\tevent=audio_mixer_changed\toperation=audio_plugin_chain\ttarget=7\t"),
        "{line}"
    );
    assert!(operation.is_live());

    // An empty chain clears one, on Master too.
    let clear = AudioMixerOperation::SetPluginChain {
        target: AudioDspTarget::Master,
        slots: Vec::new(),
    };
    let line = round_trip(&command(CommandPayload::AudioMixer(clear)));
    assert!(
        line.ends_with("\tpayload=audio_plugin_chain\ttarget=master\tplugins=\n"),
        "{line}"
    );
}

#[test]
fn malformed_plugin_chains_are_refused() {
    let prefix = "command\tprotocol=2.142\tid=a\tidempotency_key=k\tpayload=audio_plugin_chain";
    let good = format!("{prefix}\ttarget=7\tplugins=freemix.reference.trim:0:0:");
    assert!(decode_line(&format!("{good}\n")).is_ok());
    for fields in [
        // Missing target or plugins; an unknown target or plugin id.
        format!("{prefix}\tplugins=freemix.reference.trim:0:0:"),
        format!("{prefix}\ttarget=7"),
        format!("{prefix}\ttarget=0\tplugins="),
        format!("{prefix}\ttarget=x\tplugins="),
        format!("{prefix}\ttarget=7\tplugins=bad id:0:0:"),
        format!("{prefix}\ttarget=7\tplugins=bad/id:0:0:"),
        // Nine slots, a duplicate slot, a wrong part count, a bad flag, a
        // non-canonical latency, an over-long latency, and bad base64.
        format!(
            "{prefix}\ttarget=7\tplugins={}",
            (0..9)
                .map(|index| format!("plugin{index}:0:0:"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        format!(
            "{prefix}\ttarget=7\tplugins=freemix.reference.trim:0:0:,freemix.reference.trim:0:0:"
        ),
        format!("{prefix}\ttarget=7\tplugins=freemix.reference.trim:0:0"),
        format!("{prefix}\ttarget=7\tplugins=freemix.reference.trim:2:0:"),
        format!("{prefix}\ttarget=7\tplugins=freemix.reference.trim:0:00:"),
        format!("{prefix}\ttarget=7\tplugins=freemix.reference.trim:0:60001:"),
        format!("{prefix}\ttarget=7\tplugins=freemix.reference.trim:0:0:!"),
        // An unknown field.
        format!("{prefix}\ttarget=7\tplugins=\tx=1"),
    ] {
        assert!(
            decode_line(&format!("{fields}\n")).is_err(),
            "accepted {fields}"
        );
    }
    assert!(PluginChainSlot::plugin_id_is_valid(
        "freemix.reference.softclip"
    ));
    assert!(!PluginChainSlot::plugin_id_is_valid(""));
    assert!(!PluginChainSlot::plugin_id_is_valid("a:b"));
    assert_eq!(
        ProtocolVersion::new(2, 142),
        fm_protocol::CURRENT_PROTOCOL_VERSION
    );
}

#[test]
fn plugin_status_requests_and_answers_round_trip() {
    let request = WireMessage::AudioPluginStatusRequest(fm_protocol::AudioPluginStatusRequest {
        request_id: "plugin-status-7".to_owned(),
        target: AudioDspTarget::Input(input(7)),
    });
    let line = round_trip(&request);
    assert_eq!(
        line,
        "audio_plugin_status_request\trequest_id=plugin-status-7\ttarget=7\n"
    );

    let status = WireMessage::AudioPluginStatus(Box::new(fm_protocol::AudioPluginStatusMessage {
        server: fm_protocol::ServerIdentity {
            engine_id: "e".into(),
            project_id: "p".into(),
            state_epoch: 1,
            log_id: "l".into(),
        },
        request_id: "plugin-status-7".to_owned(),
        target: AudioDspTarget::Input(input(7)),
        available: vec![
            "freemix.reference.trim".to_owned(),
            "freemix.reference.polarity".to_owned(),
        ],
        slots: vec![trim(-12_000)],
    }));
    let line = round_trip(&status);
    let state = base64url(&(-12_000_i32).to_le_bytes());
    assert_eq!(
        line,
        format!(
            "audio_plugin_status\tengine_id=e\tproject_id=p\tstate_epoch=1\tlog_id=l\t\
             request_id=plugin-status-7\ttarget=7\t\
             available=freemix.reference.trim%2Cfreemix.reference.polarity\t\
             plugins=freemix.reference.trim%3A0%3A0%3A{state}\n"
        )
    );

    for fields in [
        "audio_plugin_status_request\trequest_id=p",
        "audio_plugin_status_request\trequest_id=p\ttarget=0",
        "audio_plugin_status\tengine_id=e\tproject_id=p\tstate_epoch=1\tlog_id=l\t\
         request_id=p\ttarget=7\tavailable=bad/id\tplugins=",
        "audio_plugin_status\tengine_id=e\tproject_id=p\tstate_epoch=1\tlog_id=l\t\
         request_id=p\ttarget=7\tavailable=a%2Ca\tplugins=",
        "audio_plugin_status\tengine_id=e\tproject_id=p\tstate_epoch=1\tlog_id=l\t\
         request_id=p\ttarget=7\tavailable=a\tplugins=bad/id:0:0:",
    ] {
        assert!(
            decode_line(&format!("{fields}\n")).is_err(),
            "accepted {fields}"
        );
    }
}
