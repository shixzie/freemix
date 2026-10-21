//! Protocol 2.102 audio output device list (AU-001): the
//! `audio_output_devices_request` / `audio_output_devices` exchange —
//! exactly, and strictly.

use fm_protocol::{
    AudioOutputDevice, AudioOutputDevicesMessage, AudioOutputDevicesRequest, AudioOutputInventory,
    MAX_AUDIO_OUTPUT_CHANNELS, MAX_AUDIO_OUTPUT_DESCRIPTION_BYTES, MAX_AUDIO_OUTPUT_DEVICE_BYTES,
    MAX_AUDIO_OUTPUT_DEVICES, MAX_AUDIO_OUTPUT_ERROR_BYTES, WireMessage, decode_line, encode_line,
};

fn round_trip(message: &WireMessage) -> String {
    let line = encode_line(message).unwrap();
    assert_eq!(&decode_line(&line).unwrap(), message, "{line}");
    line
}

fn refused(line: &str) {
    assert!(decode_line(line).is_err(), "accepted {line:?}");
}

fn listed(device: &str, description: &str, channels: Option<u8>) -> AudioOutputDevice {
    AudioOutputDevice {
        device: device.into(),
        description: description.into(),
        channels,
        default: false,
    }
}

fn inventory() -> AudioOutputInventory {
    let mut default = listed("default", "Default output", None);
    default.default = true;
    AudioOutputInventory {
        supported: true,
        devices: vec![
            default,
            listed("plughw:CARD=Headset,DEV=0", "Headset, USB: Audio", Some(2)),
        ],
        error: None,
    }
}

fn devices(request_id: &str, inventory: AudioOutputInventory) -> WireMessage {
    WireMessage::AudioOutputDevices(AudioOutputDevicesMessage {
        request_id: request_id.into(),
        inventory,
    })
}

/// The request and its answer have one exact form; texts travel raw after
/// their byte lengths, so names holding the list separators survive.
#[test]
fn the_output_device_exchange_is_exact() {
    let request = WireMessage::AudioOutputDevicesRequest(AudioOutputDevicesRequest {
        request_id: "ao-1".into(),
    });
    assert_eq!(
        round_trip(&request),
        "audio_output_devices_request\trequest_id=ao-1\n"
    );
    assert_eq!(
        round_trip(&devices("ao-1", inventory())),
        "audio_output_devices\trequest_id=ao-1\tsupported=1\tdevices=\
         1%7E-%7E7%7E14%3AdefaultDefault%20output\
         %2C0%7E2%7E25%7E19%3Aplughw%3ACARD%3DHeadset%2CDEV%3D0Headset%2C%20USB%3A%20Audio\n"
    );
    let mut failed = AudioOutputInventory {
        supported: true,
        devices: Vec::new(),
        error: Some("no ALSA driver is loaded".into()),
    };
    assert_eq!(
        round_trip(&devices("r", failed.clone())),
        "audio_output_devices\trequest_id=r\tsupported=1\terror=no%20ALSA%20driver%20is%20loaded\
         \tdevices=\n"
    );
    failed.devices = inventory().devices;
    round_trip(&devices("r", failed));
    assert_eq!(
        round_trip(&devices("r", AudioOutputInventory::unsupported())),
        "audio_output_devices\trequest_id=r\tsupported=0\tdevices=\n"
    );
    round_trip(&devices("r", AudioOutputInventory::simulated()));
    let mut full = inventory();
    full.devices = (0..MAX_AUDIO_OUTPUT_DEVICES)
        .map(|index| {
            let name = format!("plughw:{index:03},{}", "x".repeat(64));
            listed(
                &name[..MAX_AUDIO_OUTPUT_DEVICE_BYTES],
                &"é".repeat(MAX_AUDIO_OUTPUT_DESCRIPTION_BYTES / 2),
                Some(MAX_AUDIO_OUTPUT_CHANNELS),
            )
        })
        .collect();
    full.error = Some("é".repeat(MAX_AUDIO_OUTPUT_ERROR_BYTES / 2));
    assert_eq!(full.fitted(), full, "the longest list is already fitted");
    round_trip(&devices("r".repeat(128).as_str(), full));
}

#[test]
fn output_device_lists_that_break_the_contract_are_refused() {
    let encodes = |inventory: AudioOutputInventory| encode_line(&devices("r", inventory)).is_ok();
    let mut descending = inventory();
    descending.devices.swap(0, 1);
    assert!(!encodes(descending));
    let mut duplicate = inventory();
    duplicate.devices[1] = duplicate.devices[0].clone();
    assert!(!encodes(duplicate));
    let mut two_defaults = inventory();
    two_defaults.devices[1].default = true;
    assert!(!encodes(two_defaults));
    for name in ["-f", "two words", ""] {
        let mut bad_name = inventory();
        bad_name.devices[1].device = name.into();
        assert!(!encodes(bad_name), "{name:?}");
    }
    let mut padded = inventory();
    padded.devices[1].description = " padded".into();
    assert!(!encodes(padded));
    for channels in [0, MAX_AUDIO_OUTPUT_CHANNELS + 1] {
        let mut bad = inventory();
        bad.devices[1].channels = Some(channels);
        assert!(!encodes(bad), "{channels}");
    }
    let mut too_many = inventory();
    too_many.devices = (0..=MAX_AUDIO_OUTPUT_DEVICES)
        .map(|index| listed(&format!("hw:{index:03}"), "", None))
        .collect();
    assert!(!encodes(too_many));
    let mut unsupported_lists = inventory();
    unsupported_lists.supported = false;
    assert!(!encodes(unsupported_lists));
    let mut unsupported_error = AudioOutputInventory::unsupported();
    unsupported_error.error = Some("x".into());
    assert!(!encodes(unsupported_error));
    for text in [
        String::new(),
        "line\nbreak".into(),
        "e".repeat(MAX_AUDIO_OUTPUT_ERROR_BYTES + 1),
    ] {
        let mut bad_error = inventory();
        bad_error.error = Some(text.clone());
        assert!(!encodes(bad_error), "{text:?}");
    }

    let line = encode_line(&devices("r", inventory())).unwrap();
    for (from, to) in [
        ("supported=1", "supported=2"),
        ("supported=1", "supported=01"),
        ("devices=1%7E", "devices=2%7E"),
        ("%2C0%7E2%7E", "%2C0%7E02%7E"),
        ("%2C0%7E2%7E", "%2C0%7E0%7E"),
        ("%2C0%7E2%7E", "%2C0%7E65%7E"),
        ("%2C0%7E2%7E25", "%2C0%7E2%7E26"),
        ("%2C0%7E2%7E25", "%2C0%7E2%7E200"),
        ("%7E19%3A", "%7E20%3A"),
        ("%2C0%7E2", "%2C1%7E2"),
        ("request_id=r", "request_id="),
        ("\tdevices=", "\tdevices=%2C"),
        ("\tdevices=", "\terror=\tdevices="),
    ] {
        assert!(line.contains(from), "{from} not in {line}");
        refused(&line.replacen(from, to, 1));
    }
    refused(&line.replacen("\tsupported=1", "", 1));
    refused(&format!("{}\textra=1\n", line.trim_end()));
    refused(&format!("{},\n", line.trim_end()));
    refused("audio_output_devices_request\trequest_id=\n");
    refused("audio_output_devices_request\trequest_id=a\tsupported=1\n");
}
